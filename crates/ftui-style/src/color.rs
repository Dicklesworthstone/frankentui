//! Color types and downgrade utilities.

use std::collections::HashMap;

pub use ftui_render::ColorDepth;
use ftui_render::cell::PackedRgba;

// =============================================================================
// WCAG Contrast Validation
// =============================================================================

/// WCAG 2.0 AA contrast ratio for normal text (4.5:1).
pub const WCAG_AA_NORMAL_TEXT: f64 = 4.5;

/// WCAG 2.0 AA contrast ratio for large text (3.0:1).
pub const WCAG_AA_LARGE_TEXT: f64 = 3.0;

/// WCAG 2.0 AAA contrast ratio for normal text (7.0:1).
pub const WCAG_AAA_NORMAL_TEXT: f64 = 7.0;

/// WCAG 2.0 AAA contrast ratio for large text (4.5:1).
pub const WCAG_AAA_LARGE_TEXT: f64 = 4.5;

/// Convert sRGB channel value to linear RGB.
///
/// Per WCAG 2.0 specification for relative luminance calculation.
#[inline]
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Compute relative luminance of a color per WCAG 2.0.
///
/// Returns a value in the range [0, 1] where:
/// - 0 = pure black
/// - 1 = pure white
///
/// Formula: <https://www.w3.org/TR/WCAG20/#relativeluminancedef>
#[must_use]
pub fn relative_luminance(rgb: Rgb) -> f64 {
    let r = srgb_to_linear(rgb.r as f64 / 255.0);
    let g = srgb_to_linear(rgb.g as f64 / 255.0);
    let b = srgb_to_linear(rgb.b as f64 / 255.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Compute relative luminance of a `PackedRgba` color per WCAG 2.0.
#[must_use]
pub fn relative_luminance_packed(color: PackedRgba) -> f64 {
    relative_luminance(Rgb::from(color))
}

/// Compute WCAG 2.0 contrast ratio between two colors.
///
/// Returns a value in the range [1.0, 21.0] where:
/// - 1.0 = identical colors (no contrast)
/// - 21.0 = black on white (maximum contrast)
///
/// Formula: <https://www.w3.org/TR/WCAG20/#contrast-ratiodef>
#[must_use]
pub fn contrast_ratio(fg: Rgb, bg: Rgb) -> f64 {
    let lum_fg = relative_luminance(fg);
    let lum_bg = relative_luminance(bg);
    let lighter = lum_fg.max(lum_bg);
    let darker = lum_fg.min(lum_bg);
    (lighter + 0.05) / (darker + 0.05)
}

/// Compute WCAG 2.0 contrast ratio between two `PackedRgba` colors.
#[must_use]
pub fn contrast_ratio_packed(fg: PackedRgba, bg: PackedRgba) -> f64 {
    contrast_ratio(Rgb::from(fg), Rgb::from(bg))
}

/// Check if a color combination meets WCAG 2.0 AA for normal text (4.5:1).
#[must_use]
pub fn meets_wcag_aa(fg: Rgb, bg: Rgb) -> bool {
    contrast_ratio(fg, bg) >= WCAG_AA_NORMAL_TEXT
}

/// Check if a color combination meets WCAG 2.0 AA for normal text (4.5:1).
#[must_use]
pub fn meets_wcag_aa_packed(fg: PackedRgba, bg: PackedRgba) -> bool {
    contrast_ratio_packed(fg, bg) >= WCAG_AA_NORMAL_TEXT
}

/// Check if a color combination meets WCAG 2.0 AA for large text (3.0:1).
#[must_use]
pub fn meets_wcag_aa_large_text(fg: Rgb, bg: Rgb) -> bool {
    contrast_ratio(fg, bg) >= WCAG_AA_LARGE_TEXT
}

/// Check if a color combination meets WCAG 2.0 AAA for normal text (7.0:1).
#[must_use]
pub fn meets_wcag_aaa(fg: Rgb, bg: Rgb) -> bool {
    contrast_ratio(fg, bg) >= WCAG_AAA_NORMAL_TEXT
}

/// Select the best text color from candidates based on contrast ratio.
///
/// Returns the candidate with the highest contrast ratio against the background.
#[must_use]
pub fn best_text_color(bg: Rgb, candidates: &[Rgb]) -> Rgb {
    assert!(!candidates.is_empty(), "candidates must not be empty");

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

/// Select the best text color from candidates based on contrast ratio.
#[must_use]
pub fn best_text_color_packed(bg: PackedRgba, candidates: &[PackedRgba]) -> PackedRgba {
    assert!(!candidates.is_empty(), "candidates must not be empty");

    let mut best = candidates[0];
    let mut best_ratio = contrast_ratio_packed(best, bg);

    for &candidate in candidates.iter().skip(1) {
        let ratio = contrast_ratio_packed(candidate, bg);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
    }

    best
}

// =============================================================================
// RGB Color Type
// =============================================================================

/// RGB color (opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel (0–255).
    pub r: u8,
    /// Green channel (0–255).
    pub g: u8,
    /// Blue channel (0–255).
    pub b: u8,
}

impl Rgb {
    /// Create a new RGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Pack into a `u32` key for use in hash maps.
    #[must_use]
    pub const fn as_key(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Compute perceived luminance (BT.709) as a `u8` (0 = black, 255 = white).
    #[must_use]
    pub fn luminance_u8(self) -> u8 {
        // ITU-R BT.709 luma: 0.2126 R + 0.7152 G + 0.0722 B
        let r = self.r as u32;
        let g = self.g as u32;
        let b = self.b as u32;
        let luma = 2126 * r + 7152 * g + 722 * b;
        ((luma + 5000) / 10_000) as u8
    }
}

impl From<PackedRgba> for Rgb {
    fn from(color: PackedRgba) -> Self {
        Self::new(color.r(), color.g(), color.b())
    }
}

/// ANSI 16-color indices (0-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Ansi16 {
    /// Black (index 0).
    Black = 0,
    /// Red (index 1).
    Red = 1,
    /// Green (index 2).
    Green = 2,
    /// Yellow (index 3).
    Yellow = 3,
    /// Blue (index 4).
    Blue = 4,
    /// Magenta (index 5).
    Magenta = 5,
    /// Cyan (index 6).
    Cyan = 6,
    /// White (index 7).
    White = 7,
    /// Bright black (index 8).
    BrightBlack = 8,
    /// Bright red (index 9).
    BrightRed = 9,
    /// Bright green (index 10).
    BrightGreen = 10,
    /// Bright yellow (index 11).
    BrightYellow = 11,
    /// Bright blue (index 12).
    BrightBlue = 12,
    /// Bright magenta (index 13).
    BrightMagenta = 13,
    /// Bright cyan (index 14).
    BrightCyan = 14,
    /// Bright white (index 15).
    BrightWhite = 15,
}

impl Ansi16 {
    /// Return the raw ANSI index (0–15).
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Convert a `u8` index to an `Ansi16` variant, returning `None` if out of range.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Black),
            1 => Some(Self::Red),
            2 => Some(Self::Green),
            3 => Some(Self::Yellow),
            4 => Some(Self::Blue),
            5 => Some(Self::Magenta),
            6 => Some(Self::Cyan),
            7 => Some(Self::White),
            8 => Some(Self::BrightBlack),
            9 => Some(Self::BrightRed),
            10 => Some(Self::BrightGreen),
            11 => Some(Self::BrightYellow),
            12 => Some(Self::BrightBlue),
            13 => Some(Self::BrightMagenta),
            14 => Some(Self::BrightCyan),
            15 => Some(Self::BrightWhite),
            _ => None,
        }
    }
}

/// Monochrome output selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MonoColor {
    /// Black (dark).
    Black,
    /// White (light).
    White,
}

/// A color value at varying fidelity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// True-color RGB value.
    Rgb(Rgb),
    /// 256-color palette index.
    Ansi256(u8),
    /// Standard 16-color ANSI value.
    Ansi16(Ansi16),
    /// Monochrome (black or white).
    Mono(MonoColor),
}

impl Color {
    /// Create a true-color RGB value.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::Rgb(Rgb::new(r, g, b))
    }

    /// Convert this color to an RGB triplet regardless of its fidelity level.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        match self {
            Self::Rgb(rgb) => rgb,
            Self::Ansi256(idx) => ansi256_to_rgb(idx),
            Self::Ansi16(color) => ansi16_to_rgb(color),
            Self::Mono(MonoColor::Black) => Rgb::new(0, 0, 0),
            Self::Mono(MonoColor::White) => Rgb::new(255, 255, 255),
        }
    }

    /// Downgrade this color to fit the given color profile.
    #[must_use]
    pub fn downgrade(self, profile: ColorDepth) -> Self {
        match profile {
            ColorDepth::TrueColor => self,
            ColorDepth::Ansi256 => match self {
                Self::Rgb(rgb) => Self::Ansi256(rgb_to_256(rgb.r, rgb.g, rgb.b)),
                _ => self,
            },
            ColorDepth::Ansi16 => match self {
                Self::Rgb(rgb) => Self::Ansi16(rgb_to_ansi16(rgb.r, rgb.g, rgb.b)),
                Self::Ansi256(idx) => Self::Ansi16(rgb_to_ansi16_from_ansi256(idx)),
                _ => self,
            },
            ColorDepth::Mono => match self {
                Self::Rgb(rgb) => Self::Mono(rgb_to_mono(rgb.r, rgb.g, rgb.b)),
                Self::Ansi256(idx) => {
                    let rgb = ansi256_to_rgb(idx);
                    Self::Mono(rgb_to_mono(rgb.r, rgb.g, rgb.b))
                }
                Self::Ansi16(color) => {
                    let rgb = ansi16_to_rgb(color);
                    Self::Mono(rgb_to_mono(rgb.r, rgb.g, rgb.b))
                }
                Self::Mono(_) => self,
            },
        }
    }
}

impl From<PackedRgba> for Color {
    fn from(color: PackedRgba) -> Self {
        Self::Rgb(Rgb::from(color))
    }
}

/// Statistics for a [`ColorCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Current number of entries.
    pub size: usize,
    /// Maximum number of entries before eviction.
    pub capacity: usize,
}

/// Simple hash cache for downgrade results (bounded; clears on overflow).
#[derive(Debug)]
pub struct ColorCache {
    profile: ColorDepth,
    max_entries: usize,
    map: HashMap<u32, Color>,
    hits: u64,
    misses: u64,
}

impl ColorCache {
    /// Create a new cache with default capacity (4096 entries).
    #[must_use]
    pub fn new(profile: ColorDepth) -> Self {
        Self::with_capacity(profile, 4096)
    }

    /// Create a new cache with the given maximum entry count.
    #[must_use]
    pub fn with_capacity(profile: ColorDepth, max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self {
            profile,
            max_entries,
            map: HashMap::with_capacity(max_entries.min(2048)),
            hits: 0,
            misses: 0,
        }
    }

    /// Downgrade an RGB color through the cache, returning the cached result.
    #[must_use]
    pub fn downgrade_rgb(&mut self, rgb: Rgb) -> Color {
        let key = rgb.as_key();
        if let Some(cached) = self.map.get(&key) {
            self.hits += 1;
            return *cached;
        }
        self.misses += 1;
        let downgraded = Color::Rgb(rgb).downgrade(self.profile);
        if self.map.len() >= self.max_entries {
            self.map.clear();
        }
        self.map.insert(key, downgraded);
        downgraded
    }

    /// Downgrade a [`PackedRgba`] color through the cache.
    #[must_use]
    pub fn downgrade_packed(&mut self, color: PackedRgba) -> Color {
        self.downgrade_rgb(Rgb::from(color))
    }

    /// Return current cache statistics.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            size: self.map.len(),
            capacity: self.max_entries,
        }
    }
}

const ANSI16_PALETTE: [Rgb; 16] = [
    Rgb::new(0, 0, 0),       // Black
    Rgb::new(205, 0, 0),     // Red
    Rgb::new(0, 205, 0),     // Green
    Rgb::new(205, 205, 0),   // Yellow
    Rgb::new(0, 0, 238),     // Blue
    Rgb::new(205, 0, 205),   // Magenta
    Rgb::new(0, 205, 205),   // Cyan
    Rgb::new(229, 229, 229), // White
    Rgb::new(127, 127, 127), // Bright Black
    Rgb::new(255, 0, 0),     // Bright Red
    Rgb::new(0, 255, 0),     // Bright Green
    Rgb::new(255, 255, 0),   // Bright Yellow
    Rgb::new(92, 92, 255),   // Bright Blue
    Rgb::new(255, 0, 255),   // Bright Magenta
    Rgb::new(0, 255, 255),   // Bright Cyan
    Rgb::new(255, 255, 255), // Bright White
];

/// Convert an ANSI 16-color value to its canonical RGB representation.
#[must_use]
pub fn ansi16_to_rgb(color: Ansi16) -> Rgb {
    ANSI16_PALETTE[color.as_u8() as usize]
}

/// Convert an RGB color to a near-optimal ANSI 256-color index.
///
/// Exactly gray inputs (`r == g == b`) compare the 6×6×6 cube candidate
/// against the grayscale ramp and pick the closer; non-gray inputs map
/// straight to the cube. For *near*-gray inputs (e.g. `(128,128,129)`)
/// a grayscale-ramp entry can occasionally be marginally closer than the
/// returned cube entry — accepted for O(1) mapping without a 240-entry
/// search.
#[must_use]
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    ftui_render::ansi::rgb_to_ansi256(r, g, b)
}

/// Convert an ANSI 256-color index to its RGB representation.
#[must_use]
pub fn ansi256_to_rgb(index: u8) -> Rgb {
    if index < 16 {
        return ANSI16_PALETTE[index as usize];
    }
    if index >= 232 {
        let gray = 8 + 10 * (index - 232);
        return Rgb::new(gray, gray, gray);
    }
    let idx = index - 16;
    let r = idx / 36;
    let g = (idx / 6) % 6;
    let b = idx % 6;
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    Rgb::new(LEVELS[r as usize], LEVELS[g as usize], LEVELS[b as usize])
}

/// Convert an RGB color to the nearest ANSI 16-color value.
#[must_use]
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Ansi16 {
    let target = Rgb::new(r, g, b);
    let mut best = Ansi16::Black;
    let mut best_dist = u64::MAX;

    for (idx, candidate) in ANSI16_PALETTE.iter().enumerate() {
        let dist = weighted_distance(target, *candidate);
        if dist < best_dist {
            best = Ansi16::from_u8(idx as u8).unwrap_or(Ansi16::Black);
            best_dist = dist;
        }
    }

    best
}

/// Convert an ANSI 256-color index to the nearest ANSI 16-color value.
#[must_use]
pub fn rgb_to_ansi16_from_ansi256(index: u8) -> Ansi16 {
    let rgb = ansi256_to_rgb(index);
    rgb_to_ansi16(rgb.r, rgb.g, rgb.b)
}

/// Convert an RGB color to monochrome (black or white) based on luminance.
#[must_use]
pub fn rgb_to_mono(r: u8, g: u8, b: u8) -> MonoColor {
    let luma = Rgb::new(r, g, b).luminance_u8();
    if luma >= 128 {
        MonoColor::White
    } else {
        MonoColor::Black
    }
}

fn weighted_distance(a: Rgb, b: Rgb) -> u64 {
    let dr = a.r as i32 - b.r as i32;
    let dg = a.g as i32 - b.g as i32;
    let db = a.b as i32 - b.b as i32;
    let dr2 = (dr * dr) as u64;
    let dg2 = (dg * dg) as u64;
    let db2 = (db * db) as u64;
    2126 * dr2 + 7152 * dg2 + 722 * db2
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ColorDepth tests ---

    #[test]
    fn truecolor_passthrough() {
        let color = Color::rgb(12, 34, 56);
        assert_eq!(color.downgrade(ColorDepth::TrueColor), color);
    }

    #[test]
    fn supports_true_color() {
        assert!(ColorDepth::TrueColor.supports_true_color());
        assert!(!ColorDepth::Ansi256.supports_true_color());
        assert!(!ColorDepth::Ansi16.supports_true_color());
        assert!(!ColorDepth::Mono.supports_true_color());
    }

    #[test]
    fn supports_256_colors() {
        assert!(ColorDepth::TrueColor.supports_256_colors());
        assert!(ColorDepth::Ansi256.supports_256_colors());
        assert!(!ColorDepth::Ansi16.supports_256_colors());
        assert!(!ColorDepth::Mono.supports_256_colors());
    }

    #[test]
    fn supports_color() {
        assert!(ColorDepth::TrueColor.supports_color());
        assert!(ColorDepth::Ansi256.supports_color());
        assert!(ColorDepth::Ansi16.supports_color());
        assert!(!ColorDepth::Mono.supports_color());
    }

    // --- ColorDepth::detect_from_env tests ---

    #[test]
    fn detect_no_color_gives_mono() {
        // NO_COLOR presence (any value) should force Mono
        assert_eq!(
            ColorDepth::detect_from_env(Some("1"), None, None),
            ColorDepth::Mono
        );
        assert_eq!(
            ColorDepth::detect_from_env(Some(""), None, None),
            ColorDepth::Mono
        );
        // NO_COLOR takes precedence over COLORTERM
        assert_eq!(
            ColorDepth::detect_from_env(Some("1"), Some("truecolor"), Some("xterm-256color")),
            ColorDepth::Mono
        );
    }

    #[test]
    fn detect_colorterm_truecolor() {
        assert_eq!(
            ColorDepth::detect_from_env(None, Some("truecolor"), Some("xterm")),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn detect_colorterm_24bit() {
        assert_eq!(
            ColorDepth::detect_from_env(None, Some("24bit"), Some("xterm")),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn detect_term_256color() {
        assert_eq!(
            ColorDepth::detect_from_env(None, None, Some("xterm-256color")),
            ColorDepth::Ansi256
        );
        assert_eq!(
            ColorDepth::detect_from_env(None, None, Some("screen-256color")),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn detect_colorterm_unknown_falls_to_term() {
        // COLORTERM=yes is not truecolor, so fall back to TERM
        assert_eq!(
            ColorDepth::detect_from_env(None, Some("yes"), Some("xterm-256color")),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn detect_conservative_fallbacks() {
        assert_eq!(
            ColorDepth::detect_from_env(None, None, None),
            ColorDepth::Mono
        );
        assert_eq!(
            ColorDepth::detect_from_env(None, None, Some("xterm")),
            ColorDepth::Ansi16
        );
        assert_eq!(
            ColorDepth::detect_from_env(None, Some(""), Some("dumb")),
            ColorDepth::Mono
        );
        assert_eq!(
            ColorDepth::detect_from_env(None, Some("truecolor"), Some("vt100")),
            ColorDepth::Mono
        );
    }

    // --- WCAG Contrast tests ---

    #[test]
    fn wcag_luminance_black_is_zero() {
        let lum = relative_luminance(Rgb::new(0, 0, 0));
        assert!((lum - 0.0).abs() < 0.001);
    }

    #[test]
    fn wcag_luminance_white_is_one() {
        let lum = relative_luminance(Rgb::new(255, 255, 255));
        assert!((lum - 1.0).abs() < 0.001);
    }

    #[test]
    fn wcag_luminance_green_is_brightest() {
        // Green has highest weight (0.7152) in luminance formula
        let r_lum = relative_luminance(Rgb::new(255, 0, 0));
        let g_lum = relative_luminance(Rgb::new(0, 255, 0));
        let b_lum = relative_luminance(Rgb::new(0, 0, 255));
        assert!(g_lum > r_lum);
        assert!(g_lum > b_lum);
    }

    #[test]
    fn contrast_ratio_black_white_is_21() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        let ratio = contrast_ratio(black, white);
        // Should be exactly 21:1
        assert!((ratio - 21.0).abs() < 0.01, "ratio was {}", ratio);
    }

    #[test]
    fn contrast_ratio_is_symmetric() {
        let a = Rgb::new(100, 150, 200);
        let b = Rgb::new(50, 75, 100);
        let ratio_ab = contrast_ratio(a, b);
        let ratio_ba = contrast_ratio(b, a);
        assert!((ratio_ab - ratio_ba).abs() < 0.001);
    }

    #[test]
    fn contrast_ratio_same_color_is_one() {
        let color = Rgb::new(128, 128, 128);
        let ratio = contrast_ratio(color, color);
        assert!((ratio - 1.0).abs() < 0.001);
    }

    #[test]
    fn meets_wcag_aa_black_white() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!(meets_wcag_aa(black, white));
        assert!(meets_wcag_aa(white, black));
    }

    #[test]
    fn meets_wcag_aa_low_contrast_fails() {
        // Two similar grays should fail WCAG AA
        let gray1 = Rgb::new(128, 128, 128);
        let gray2 = Rgb::new(140, 140, 140);
        assert!(!meets_wcag_aa(gray1, gray2));
    }

    #[test]
    fn meets_wcag_aaa_black_white() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!(meets_wcag_aaa(black, white));
    }

    #[test]
    fn best_text_color_chooses_highest_contrast() {
        let dark_bg = Rgb::new(30, 30, 30);
        let candidates = [
            Rgb::new(50, 50, 50),    // dark gray - low contrast
            Rgb::new(255, 255, 255), // white - high contrast
            Rgb::new(100, 100, 100), // mid gray - medium contrast
        ];
        let best = best_text_color(dark_bg, &candidates);
        assert_eq!(best, Rgb::new(255, 255, 255));

        let light_bg = Rgb::new(240, 240, 240);
        let best_on_light = best_text_color(light_bg, &candidates);
        assert_eq!(best_on_light, Rgb::new(50, 50, 50));
    }

    #[test]
    fn wcag_constants_are_correct() {
        assert!((WCAG_AA_NORMAL_TEXT - 4.5).abs() < 0.001);
        assert!((WCAG_AA_LARGE_TEXT - 3.0).abs() < 0.001);
        assert!((WCAG_AAA_NORMAL_TEXT - 7.0).abs() < 0.001);
        assert!((WCAG_AAA_LARGE_TEXT - 4.5).abs() < 0.001);
    }

    // --- Rgb tests ---

    #[test]
    fn rgb_as_key_is_unique() {
        let a = Rgb::new(1, 2, 3);
        let b = Rgb::new(3, 2, 1);
        assert_ne!(a.as_key(), b.as_key());
        assert_eq!(a.as_key(), Rgb::new(1, 2, 3).as_key());
    }

    #[test]
    fn rgb_luminance_black_is_zero() {
        assert_eq!(Rgb::new(0, 0, 0).luminance_u8(), 0);
    }

    #[test]
    fn rgb_luminance_white_is_255() {
        assert_eq!(Rgb::new(255, 255, 255).luminance_u8(), 255);
    }

    #[test]
    fn rgb_luminance_green_is_brightest_channel() {
        // Green has highest weight in BT.709 luma
        let green_only = Rgb::new(0, 128, 0).luminance_u8();
        let red_only = Rgb::new(128, 0, 0).luminance_u8();
        let blue_only = Rgb::new(0, 0, 128).luminance_u8();
        assert!(green_only > red_only);
        assert!(green_only > blue_only);
    }

    #[test]
    fn rgb_from_packed_rgba() {
        let packed = PackedRgba::rgb(10, 20, 30);
        let rgb: Rgb = packed.into();
        assert_eq!(rgb, Rgb::new(10, 20, 30));
    }

    #[test]
    fn relative_luminance_packed_ignores_alpha() {
        let opaque = PackedRgba::rgba(10, 20, 30, 255);
        let transparent = PackedRgba::rgba(10, 20, 30, 0);
        let l1 = relative_luminance_packed(opaque);
        let l2 = relative_luminance_packed(transparent);
        assert!(
            (l1 - l2).abs() < 1.0e-12,
            "luminance should ignore alpha (l1={l1}, l2={l2})"
        );
    }

    // --- Ansi16 tests ---

    #[test]
    fn ansi16_from_u8_valid_range() {
        for i in 0..=15 {
            assert!(Ansi16::from_u8(i).is_some());
        }
    }

    #[test]
    fn ansi16_from_u8_invalid() {
        assert!(Ansi16::from_u8(16).is_none());
        assert!(Ansi16::from_u8(255).is_none());
    }

    #[test]
    fn ansi16_round_trip() {
        for i in 0..=15 {
            let color = Ansi16::from_u8(i).unwrap();
            assert_eq!(color.as_u8(), i);
        }
    }

    // --- rgb_to_256 tests ---

    #[test]
    fn rgb_to_256_grayscale_rules() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(8, 8, 8), 232);
        assert_eq!(rgb_to_256(18, 18, 18), 233);
        assert_eq!(rgb_to_256(249, 249, 249), 231);
    }

    #[test]
    fn rgb_to_256_primary_red() {
        assert_eq!(rgb_to_256(255, 0, 0), 196);
    }

    #[test]
    fn rgb_to_256_primary_green() {
        assert_eq!(rgb_to_256(0, 255, 0), 46);
    }

    #[test]
    fn rgb_to_256_primary_blue() {
        assert_eq!(rgb_to_256(0, 0, 255), 21);
    }

    // --- ansi256_to_rgb tests ---

    #[test]
    fn ansi256_to_rgb_round_trip() {
        let rgb = ansi256_to_rgb(196);
        assert_eq!(rgb, Rgb::new(255, 0, 0));
    }

    #[test]
    fn ansi256_to_rgb_first_16_match_palette() {
        for i in 0..16 {
            let rgb = ansi256_to_rgb(i);
            assert_eq!(rgb, ANSI16_PALETTE[i as usize]);
        }
    }

    #[test]
    fn ansi256_to_rgb_grayscale_ramp() {
        // Index 232 = darkest gray (8,8,8), 255 = lightest (238,238,238)
        let darkest = ansi256_to_rgb(232);
        assert_eq!(darkest, Rgb::new(8, 8, 8));
        let lightest = ansi256_to_rgb(255);
        assert_eq!(lightest, Rgb::new(238, 238, 238));
    }

    #[test]
    fn ansi256_color_cube_corners() {
        // Index 16 = (0,0,0) in cube
        assert_eq!(ansi256_to_rgb(16), Rgb::new(0, 0, 0));
        // Index 231 = (255,255,255) in cube
        assert_eq!(ansi256_to_rgb(231), Rgb::new(255, 255, 255));
    }

    // --- rgb_to_ansi16 tests ---

    #[test]
    fn rgb_to_ansi16_basics() {
        assert_eq!(rgb_to_ansi16(0, 0, 0), Ansi16::Black);
        assert_eq!(rgb_to_ansi16(255, 0, 0), Ansi16::BrightRed);
        assert_eq!(rgb_to_ansi16(0, 255, 0), Ansi16::BrightGreen);
        assert_eq!(rgb_to_ansi16(0, 0, 255), Ansi16::Blue);
    }

    #[test]
    fn rgb_to_ansi16_white() {
        assert_eq!(rgb_to_ansi16(255, 255, 255), Ansi16::BrightWhite);
    }

    // --- rgb_to_mono tests ---

    #[test]
    fn mono_fallback() {
        assert_eq!(rgb_to_mono(0, 0, 0), MonoColor::Black);
        assert_eq!(rgb_to_mono(255, 255, 255), MonoColor::White);
        assert_eq!(rgb_to_mono(200, 200, 200), MonoColor::White);
        assert_eq!(rgb_to_mono(30, 30, 30), MonoColor::Black);
    }

    #[test]
    fn mono_boundary() {
        // Luminance threshold is 128
        assert_eq!(rgb_to_mono(128, 128, 128), MonoColor::White);
        assert_eq!(rgb_to_mono(127, 127, 127), MonoColor::Black);
    }

    // --- Color downgrade chain tests ---

    #[test]
    fn downgrade_rgb_to_ansi256() {
        let color = Color::rgb(255, 0, 0);
        let downgraded = color.downgrade(ColorDepth::Ansi256);
        assert!(matches!(downgraded, Color::Ansi256(_)));
    }

    #[test]
    fn downgrade_rgb_to_ansi16() {
        let color = Color::rgb(255, 0, 0);
        let downgraded = color.downgrade(ColorDepth::Ansi16);
        assert!(matches!(downgraded, Color::Ansi16(_)));
    }

    #[test]
    fn downgrade_rgb_to_mono() {
        let color = Color::rgb(255, 255, 255);
        let downgraded = color.downgrade(ColorDepth::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::White));
    }

    #[test]
    fn downgrade_ansi256_to_ansi16() {
        let color = Color::Ansi256(196);
        let downgraded = color.downgrade(ColorDepth::Ansi16);
        assert!(matches!(downgraded, Color::Ansi16(_)));
    }

    #[test]
    fn downgrade_ansi256_to_mono() {
        let color = Color::Ansi256(232); // dark gray
        let downgraded = color.downgrade(ColorDepth::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::Black));
    }

    #[test]
    fn downgrade_ansi16_to_mono() {
        let color = Color::Ansi16(Ansi16::BrightWhite);
        let downgraded = color.downgrade(ColorDepth::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::White));
    }

    #[test]
    fn downgrade_mono_stays_mono() {
        let color = Color::Mono(MonoColor::Black);
        assert_eq!(color.downgrade(ColorDepth::Mono), color);
    }

    #[test]
    fn downgrade_ansi16_stays_at_ansi256() {
        let color = Color::Ansi16(Ansi16::Red);
        // Ansi16 should pass through at Ansi256 level
        assert_eq!(color.downgrade(ColorDepth::Ansi256), color);
    }

    // --- Color::to_rgb tests ---

    #[test]
    fn color_to_rgb_all_variants() {
        assert_eq!(Color::rgb(1, 2, 3).to_rgb(), Rgb::new(1, 2, 3));
        assert_eq!(Color::Ansi256(196).to_rgb(), Rgb::new(255, 0, 0));
        assert_eq!(Color::Ansi16(Ansi16::Black).to_rgb(), Rgb::new(0, 0, 0));
        assert_eq!(
            Color::Mono(MonoColor::White).to_rgb(),
            Rgb::new(255, 255, 255)
        );
        assert_eq!(Color::Mono(MonoColor::Black).to_rgb(), Rgb::new(0, 0, 0));
    }

    // --- Color from PackedRgba ---

    #[test]
    fn color_from_packed_rgba() {
        let packed = PackedRgba::rgb(42, 84, 126);
        let color: Color = packed.into();
        assert_eq!(color, Color::Rgb(Rgb::new(42, 84, 126)));
    }

    #[test]
    fn color_from_packed_rgba_ignores_alpha() {
        let packed = PackedRgba::rgba(42, 84, 126, 10);
        let color: Color = packed.into();
        assert_eq!(color, Color::Rgb(Rgb::new(42, 84, 126)));
    }

    // --- ColorCache tests ---

    #[test]
    fn cache_tracks_hits() {
        let mut cache = ColorCache::with_capacity(ColorDepth::Ansi16, 8);
        let rgb = Rgb::new(10, 20, 30);
        let _ = cache.downgrade_rgb(rgb);
        let _ = cache.downgrade_rgb(rgb);
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.size, 1);
    }

    #[test]
    fn cache_clears_on_overflow() {
        let mut cache = ColorCache::with_capacity(ColorDepth::Ansi16, 2);
        let _ = cache.downgrade_rgb(Rgb::new(1, 0, 0));
        let _ = cache.downgrade_rgb(Rgb::new(2, 0, 0));
        assert_eq!(cache.stats().size, 2);
        // Third entry should trigger clear
        let _ = cache.downgrade_rgb(Rgb::new(3, 0, 0));
        assert_eq!(cache.stats().size, 1);
    }

    #[test]
    fn cache_downgrade_packed() {
        let mut cache = ColorCache::with_capacity(ColorDepth::Ansi16, 8);
        let packed = PackedRgba::rgb(255, 0, 0);
        let result = cache.downgrade_packed(packed);
        assert!(matches!(result, Color::Ansi16(_)));
    }

    #[test]
    fn cache_default_capacity() {
        let cache = ColorCache::new(ColorDepth::TrueColor);
        assert_eq!(cache.stats().capacity, 4096);
    }

    #[test]
    fn cache_minimum_capacity_is_one() {
        let cache = ColorCache::with_capacity(ColorDepth::Ansi16, 0);
        assert_eq!(cache.stats().capacity, 1);
    }
}

#[cfg(test)]
mod downgrade_edge_cases {
    //! Tests for color downgrade edge cases and boundary conditions.
    //!
    //! These tests verify correct behavior at palette boundaries,
    //! grayscale thresholds, and the sequential downgrade pipeline.

    use super::*;

    // =========================================================================
    // Sequential downgrade verification
    // =========================================================================

    #[test]
    fn sequential_downgrade_truecolor_to_mono() {
        // Verify the full downgrade pipeline: RGB -> 256 -> 16 -> Mono
        let white = Color::rgb(255, 255, 255);
        let black = Color::rgb(0, 0, 0);

        // White through all stages
        let w256 = white.downgrade(ColorDepth::Ansi256);
        assert!(matches!(w256, Color::Ansi256(231))); // Pure white in cube
        let w16 = w256.downgrade(ColorDepth::Ansi16);
        assert!(matches!(w16, Color::Ansi16(Ansi16::BrightWhite)));
        let wmono = w16.downgrade(ColorDepth::Mono);
        assert_eq!(wmono, Color::Mono(MonoColor::White));

        // Black through all stages
        let b256 = black.downgrade(ColorDepth::Ansi256);
        assert!(matches!(b256, Color::Ansi256(16))); // Pure black
        let b16 = b256.downgrade(ColorDepth::Ansi16);
        assert!(matches!(b16, Color::Ansi16(Ansi16::Black)));
        let bmono = b16.downgrade(ColorDepth::Mono);
        assert_eq!(bmono, Color::Mono(MonoColor::Black));
    }

    #[test]
    fn sequential_downgrade_preserves_intent() {
        // Red should stay "reddish" through the pipeline
        let red = Color::rgb(255, 0, 0);

        let r256 = red.downgrade(ColorDepth::Ansi256);
        let Color::Ansi256(idx) = r256 else {
            panic!("Expected Ansi256");
        };
        assert_eq!(idx, 196); // Pure red in 256-color

        let r16 = r256.downgrade(ColorDepth::Ansi16);
        let Color::Ansi16(ansi) = r16 else {
            panic!("Expected Ansi16");
        };
        // Should map to BrightRed (not some other color)
        assert_eq!(ansi, Ansi16::BrightRed);
    }

    // =========================================================================
    // rgb_to_256 edge cases
    // =========================================================================

    #[test]
    fn rgb_to_256_grayscale_boundaries() {
        // Test exact boundary values for grayscale detection
        // Values through the midpoint between 0 and 8 use cube black.
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(4, 4, 4), 16);

        // Values above that midpoint use the grayscale ramp.
        assert_eq!(rgb_to_256(5, 5, 5), 232);
        assert_eq!(rgb_to_256(7, 7, 7), 232);
        assert_eq!(rgb_to_256(8, 8, 8), 232);
        assert_eq!(rgb_to_256(17, 17, 17), 233);

        // r > 246 -> 231 (white in cube, nearest neighbor for values
        // closer to 255 than to 238, the last grayscale ramp entry)
        assert_eq!(rgb_to_256(247, 247, 247), 231);
        assert_eq!(rgb_to_256(248, 248, 248), 231);
        assert_eq!(rgb_to_256(249, 249, 249), 231);
        assert_eq!(rgb_to_256(255, 255, 255), 231);

        // r = 246 is still in grayscale ramp (closer to 238 than 255)
        assert_eq!(rgb_to_256(246, 246, 246), 255);
    }

    #[test]
    fn rgb_to_256_grayscale_ramp_coverage() {
        // Grayscale ramp 232-255 covers values 8-238
        // Each step is 10 units: 8, 18, 28, ..., 238
        for i in 0..24 {
            let gray_val = 8 + i * 10;
            let idx = rgb_to_256(gray_val, gray_val, gray_val);
            assert!(
                (232..=255).contains(&idx),
                "Gray {} mapped to {} (expected 232-255)",
                gray_val,
                idx
            );
        }
    }

    #[test]
    fn rgb_to_256_cube_corners() {
        // Test all 8 corners of the 6x6x6 RGB cube
        assert_eq!(rgb_to_256(0, 0, 0), 16); // Handled as grayscale
        assert_eq!(rgb_to_256(255, 0, 0), 196); // Red corner
        assert_eq!(rgb_to_256(0, 255, 0), 46); // Green corner
        assert_eq!(rgb_to_256(0, 0, 255), 21); // Blue corner
        assert_eq!(rgb_to_256(255, 255, 0), 226); // Yellow corner
        assert_eq!(rgb_to_256(255, 0, 255), 201); // Magenta corner
        assert_eq!(rgb_to_256(0, 255, 255), 51); // Cyan corner
        // White handled as grayscale, maps to 231
        assert_eq!(rgb_to_256(255, 255, 255), 231);
    }

    #[test]
    fn rgb_to_256_non_gray_avoids_grayscale() {
        // When channels differ, should use cube even if values are gray-ish
        // r=100, g=100, b=99 is NOT grayscale (not all equal)
        let idx = rgb_to_256(100, 100, 99);
        // Should be in cube range (16-231), not grayscale (232-255)
        assert!(
            (16..=231).contains(&idx),
            "Non-gray {} should use cube",
            idx
        );
    }

    // =========================================================================
    // ansi256_to_rgb edge cases
    // =========================================================================

    #[test]
    fn ansi256_to_rgb_full_range() {
        // Every index should produce valid RGB (this is a sanity check)
        for i in 0..=255 {
            let rgb = ansi256_to_rgb(i);
            // Verify the values are reasonable (non-panic)
            let _ = (rgb.r, rgb.g, rgb.b);
        }
    }

    #[test]
    fn ansi256_to_rgb_grayscale_range() {
        // Indices 232-255 should produce grayscale (r=g=b)
        for i in 232..=255 {
            let rgb = ansi256_to_rgb(i);
            assert_eq!(rgb.r, rgb.g);
            assert_eq!(rgb.g, rgb.b);
        }
    }

    #[test]
    fn ansi256_to_rgb_first_16_are_palette() {
        // Indices 0-15 should use the 16-color palette
        for i in 0..16 {
            let rgb = ansi256_to_rgb(i);
            assert_eq!(rgb, ANSI16_PALETTE[i as usize]);
        }
    }

    // =========================================================================
    // rgb_to_ansi16 edge cases
    // =========================================================================

    #[test]
    fn rgb_to_ansi16_pure_primaries() {
        // Pure primaries should map to their bright variants
        assert_eq!(rgb_to_ansi16(255, 0, 0), Ansi16::BrightRed);
        assert_eq!(rgb_to_ansi16(0, 255, 0), Ansi16::BrightGreen);
        // Blue maps to regular Blue because the bright blue in palette is different
        assert_eq!(rgb_to_ansi16(0, 0, 255), Ansi16::Blue);
    }

    #[test]
    fn rgb_to_ansi16_grays() {
        // Dark gray should map to BrightBlack (127,127,127)
        assert_eq!(rgb_to_ansi16(127, 127, 127), Ansi16::BrightBlack);
        // Mid gray closer to White (229,229,229)
        assert_eq!(rgb_to_ansi16(200, 200, 200), Ansi16::White);
    }

    #[test]
    fn rgb_to_ansi16_extremes() {
        // Pure black and white
        assert_eq!(rgb_to_ansi16(0, 0, 0), Ansi16::Black);
        assert_eq!(rgb_to_ansi16(255, 255, 255), Ansi16::BrightWhite);
    }

    // =========================================================================
    // rgb_to_mono edge cases
    // =========================================================================

    #[test]
    fn rgb_to_mono_luminance_boundary() {
        // Luminance threshold is 128
        // Test values near the boundary
        assert_eq!(rgb_to_mono(128, 128, 128), MonoColor::White);
        assert_eq!(rgb_to_mono(127, 127, 127), MonoColor::Black);

        // Test with weighted luminance (green has highest weight)
        // Green at ~180 should give luminance ~128 (0.7152 * 180 = 128.7)
        assert_eq!(rgb_to_mono(0, 180, 0), MonoColor::White);
        assert_eq!(rgb_to_mono(0, 178, 0), MonoColor::Black);
    }

    #[test]
    fn rgb_to_mono_color_saturation_irrelevant() {
        // Mono cares only about luminance, not saturation
        // Pure red (luma = 0.2126 * 255 = 54) -> black
        assert_eq!(rgb_to_mono(255, 0, 0), MonoColor::Black);
        // Pure green (luma = 0.7152 * 255 = 182) -> white
        assert_eq!(rgb_to_mono(0, 255, 0), MonoColor::White);
        // Pure blue (luma = 0.0722 * 255 = 18) -> black
        assert_eq!(rgb_to_mono(0, 0, 255), MonoColor::Black);
    }

    // =========================================================================
    // Color downgrade stability tests
    // =========================================================================

    #[test]
    fn downgrade_at_same_level_is_identity() {
        // Downgrading to the same level should not change the color
        let ansi16 = Color::Ansi16(Ansi16::Red);
        assert_eq!(ansi16.downgrade(ColorDepth::Ansi16), ansi16);

        let ansi256 = Color::Ansi256(100);
        assert_eq!(ansi256.downgrade(ColorDepth::Ansi256), ansi256);

        let mono = Color::Mono(MonoColor::Black);
        assert_eq!(mono.downgrade(ColorDepth::Mono), mono);

        let rgb = Color::rgb(1, 2, 3);
        assert_eq!(rgb.downgrade(ColorDepth::TrueColor), rgb);
    }

    #[test]
    fn downgrade_ansi16_passes_through_ansi256() {
        // Ansi16 should not change when downgraded to Ansi256
        // (it's already "lower fidelity")
        let color = Color::Ansi16(Ansi16::Cyan);
        assert_eq!(color.downgrade(ColorDepth::Ansi256), color);
    }

    #[test]
    fn downgrade_mono_passes_through_all() {
        // Mono should never change
        let black = Color::Mono(MonoColor::Black);
        let white = Color::Mono(MonoColor::White);

        assert_eq!(black.downgrade(ColorDepth::TrueColor), black);
        assert_eq!(black.downgrade(ColorDepth::Ansi256), black);
        assert_eq!(black.downgrade(ColorDepth::Ansi16), black);
        assert_eq!(black.downgrade(ColorDepth::Mono), black);

        assert_eq!(white.downgrade(ColorDepth::TrueColor), white);
        assert_eq!(white.downgrade(ColorDepth::Ansi256), white);
        assert_eq!(white.downgrade(ColorDepth::Ansi16), white);
        assert_eq!(white.downgrade(ColorDepth::Mono), white);
    }

    // =========================================================================
    // Luminance edge cases
    // =========================================================================

    #[test]
    fn luminance_formula_correctness() {
        // BT.709: 0.2126 R + 0.7152 G + 0.0722 B
        // Pure channels
        let r_luma = Rgb::new(255, 0, 0).luminance_u8();
        let g_luma = Rgb::new(0, 255, 0).luminance_u8();
        let b_luma = Rgb::new(0, 0, 255).luminance_u8();

        // Red: 0.2126 * 255 = 54.2
        assert!(
            (50..=58).contains(&r_luma),
            "Red luma {} not near 54",
            r_luma
        );
        // Green: 0.7152 * 255 = 182.4
        assert!(
            (178..=186).contains(&g_luma),
            "Green luma {} not near 182",
            g_luma
        );
        // Blue: 0.0722 * 255 = 18.4
        assert!(
            (15..=22).contains(&b_luma),
            "Blue luma {} not near 18",
            b_luma
        );

        // Combined should match
        let all = Rgb::new(255, 255, 255).luminance_u8();
        assert_eq!(all, 255);
    }

    #[test]
    fn luminance_mid_values() {
        // Test some mid-range values
        let mid_gray = Rgb::new(128, 128, 128).luminance_u8();
        // Should be approximately 128
        assert!(
            (126..=130).contains(&mid_gray),
            "Mid gray luma {} not near 128",
            mid_gray
        );
    }
}
