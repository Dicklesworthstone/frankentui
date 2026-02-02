use std::collections::HashMap;

use ftui_render::cell::PackedRgba;

/// Terminal color profile used for downgrade decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorProfile {
    Mono,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl ColorProfile {
    /// Choose the best available profile from detection flags.
    ///
    /// `no_color` should reflect explicit user intent (e.g. NO_COLOR).
    #[must_use]
    pub const fn from_flags(true_color: bool, colors_256: bool, no_color: bool) -> Self {
        if no_color {
            Self::Mono
        } else if true_color {
            Self::TrueColor
        } else if colors_256 {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    #[must_use]
    pub const fn supports_true_color(self) -> bool {
        matches!(self, Self::TrueColor)
    }
}

/// RGB color (opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub const fn as_key(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

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
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    Blue = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
    BrightBlack = 8,
    BrightRed = 9,
    BrightGreen = 10,
    BrightYellow = 11,
    BrightBlue = 12,
    BrightMagenta = 13,
    BrightCyan = 14,
    BrightWhite = 15,
}

impl Ansi16 {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

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
    Black,
    White,
}

/// A color value at varying fidelity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Rgb(Rgb),
    Ansi256(u8),
    Ansi16(Ansi16),
    Mono(MonoColor),
}

impl Color {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::Rgb(Rgb::new(r, g, b))
    }

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

    #[must_use]
    pub fn downgrade(self, profile: ColorProfile) -> Self {
        match profile {
            ColorProfile::TrueColor => self,
            ColorProfile::Ansi256 => match self {
                Self::Rgb(rgb) => Self::Ansi256(rgb_to_256(rgb.r, rgb.g, rgb.b)),
                _ => self,
            },
            ColorProfile::Ansi16 => match self {
                Self::Rgb(rgb) => Self::Ansi16(rgb_to_ansi16(rgb.r, rgb.g, rgb.b)),
                Self::Ansi256(idx) => Self::Ansi16(rgb_to_ansi16_from_ansi256(idx)),
                _ => self,
            },
            ColorProfile::Mono => match self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub size: usize,
    pub capacity: usize,
}

/// Simple hash cache for downgrade results (bounded; clears on overflow).
#[derive(Debug)]
pub struct ColorCache {
    profile: ColorProfile,
    max_entries: usize,
    map: HashMap<u32, Color>,
    hits: u64,
    misses: u64,
}

impl ColorCache {
    #[must_use]
    pub fn new(profile: ColorProfile) -> Self {
        Self::with_capacity(profile, 4096)
    }

    #[must_use]
    pub fn with_capacity(profile: ColorProfile, max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self {
            profile,
            max_entries,
            map: HashMap::with_capacity(max_entries.min(2048)),
            hits: 0,
            misses: 0,
        }
    }

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

    #[must_use]
    pub fn downgrade_packed(&mut self, color: PackedRgba) -> Color {
        self.downgrade_rgb(Rgb::from(color))
    }

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

#[must_use]
pub fn ansi16_to_rgb(color: Ansi16) -> Rgb {
    ANSI16_PALETTE[color.as_u8() as usize]
}

#[must_use]
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        let idx = ((r - 8) / 10).min(23);
        return 232 + idx;
    }

    let r6 = (r as u16 * 6 / 256) as u8;
    let g6 = (g as u16 * 6 / 256) as u8;
    let b6 = (b as u16 * 6 / 256) as u8;
    16 + 36 * r6 + 6 * g6 + b6
}

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

#[must_use]
pub fn rgb_to_ansi16_from_ansi256(index: u8) -> Ansi16 {
    let rgb = ansi256_to_rgb(index);
    rgb_to_ansi16(rgb.r, rgb.g, rgb.b)
}

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

    // --- ColorProfile tests ---

    #[test]
    fn truecolor_passthrough() {
        let color = Color::rgb(12, 34, 56);
        assert_eq!(color.downgrade(ColorProfile::TrueColor), color);
    }

    #[test]
    fn profile_from_flags_prefers_mono() {
        assert_eq!(
            ColorProfile::from_flags(true, true, true),
            ColorProfile::Mono
        );
        assert_eq!(
            ColorProfile::from_flags(true, false, false),
            ColorProfile::TrueColor
        );
        assert_eq!(
            ColorProfile::from_flags(false, true, false),
            ColorProfile::Ansi256
        );
        assert_eq!(
            ColorProfile::from_flags(false, false, false),
            ColorProfile::Ansi16
        );
    }

    #[test]
    fn supports_true_color() {
        assert!(ColorProfile::TrueColor.supports_true_color());
        assert!(!ColorProfile::Ansi256.supports_true_color());
        assert!(!ColorProfile::Ansi16.supports_true_color());
        assert!(!ColorProfile::Mono.supports_true_color());
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
        let downgraded = color.downgrade(ColorProfile::Ansi256);
        assert!(matches!(downgraded, Color::Ansi256(_)));
    }

    #[test]
    fn downgrade_rgb_to_ansi16() {
        let color = Color::rgb(255, 0, 0);
        let downgraded = color.downgrade(ColorProfile::Ansi16);
        assert!(matches!(downgraded, Color::Ansi16(_)));
    }

    #[test]
    fn downgrade_rgb_to_mono() {
        let color = Color::rgb(255, 255, 255);
        let downgraded = color.downgrade(ColorProfile::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::White));
    }

    #[test]
    fn downgrade_ansi256_to_ansi16() {
        let color = Color::Ansi256(196);
        let downgraded = color.downgrade(ColorProfile::Ansi16);
        assert!(matches!(downgraded, Color::Ansi16(_)));
    }

    #[test]
    fn downgrade_ansi256_to_mono() {
        let color = Color::Ansi256(232); // dark gray
        let downgraded = color.downgrade(ColorProfile::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::Black));
    }

    #[test]
    fn downgrade_ansi16_to_mono() {
        let color = Color::Ansi16(Ansi16::BrightWhite);
        let downgraded = color.downgrade(ColorProfile::Mono);
        assert_eq!(downgraded, Color::Mono(MonoColor::White));
    }

    #[test]
    fn downgrade_mono_stays_mono() {
        let color = Color::Mono(MonoColor::Black);
        assert_eq!(color.downgrade(ColorProfile::Mono), color);
    }

    #[test]
    fn downgrade_ansi16_stays_at_ansi256() {
        let color = Color::Ansi16(Ansi16::Red);
        // Ansi16 should pass through at Ansi256 level
        assert_eq!(color.downgrade(ColorProfile::Ansi256), color);
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

    // --- ColorCache tests ---

    #[test]
    fn cache_tracks_hits() {
        let mut cache = ColorCache::with_capacity(ColorProfile::Ansi16, 8);
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
        let mut cache = ColorCache::with_capacity(ColorProfile::Ansi16, 2);
        cache.downgrade_rgb(Rgb::new(1, 0, 0));
        cache.downgrade_rgb(Rgb::new(2, 0, 0));
        assert_eq!(cache.stats().size, 2);
        // Third entry should trigger clear
        cache.downgrade_rgb(Rgb::new(3, 0, 0));
        assert_eq!(cache.stats().size, 1);
    }

    #[test]
    fn cache_downgrade_packed() {
        let mut cache = ColorCache::with_capacity(ColorProfile::Ansi16, 8);
        let packed = PackedRgba::rgb(255, 0, 0);
        let result = cache.downgrade_packed(packed);
        assert!(matches!(result, Color::Ansi16(_)));
    }

    #[test]
    fn cache_default_capacity() {
        let cache = ColorCache::new(ColorProfile::TrueColor);
        assert_eq!(cache.stats().capacity, 4096);
    }

    #[test]
    fn cache_minimum_capacity_is_one() {
        let cache = ColorCache::with_capacity(ColorProfile::Ansi16, 0);
        assert_eq!(cache.stats().capacity, 1);
    }
}
