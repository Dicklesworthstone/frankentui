//! Doom palette (PLAYPAL) and light mapping (COLORMAP).
//!
//! Provides color lookup from palette indices with distance-based light diminishing.

/// Doom's palette: 256 RGB colors.
#[derive(Debug, Clone)]
pub struct DoomPalette {
    /// 256 RGB triplets from PLAYPAL.
    pub colors: Vec<[u8; 3]>,
    /// 34 colormaps for light levels (0 = brightest, 31 = darkest, 32-33 = special).
    pub colormaps: Vec<Vec<u8>>,
}

/// The default Doom palette (first 16 colors approximation for when no WAD is loaded).
const DEFAULT_PALETTE: [[u8; 3]; 256] = {
    let mut pal = [[0u8; 3]; 256];
    // We'll fill this with a reasonable gradient at compile time
    let mut i = 0;
    while i < 256 {
        // Simple grayscale + hue variation for a reasonable default
        let r = i as u8;
        let g = i as u8;
        let b = i as u8;
        pal[i] = [r, g, b];
        i += 1;
    }
    pal
};

impl DoomPalette {
    /// Create a palette from WAD data.
    pub fn from_wad(colors: Vec<[u8; 3]>, colormaps: Vec<Vec<u8>>) -> Self {
        Self { colors, colormaps }
    }

    /// Create a default grayscale palette.
    pub fn default_palette() -> Self {
        Self {
            colors: DEFAULT_PALETTE.to_vec(),
            colormaps: Self::generate_default_colormaps(),
        }
    }

    /// Generate simple light-diminishing colormaps.
    fn generate_default_colormaps() -> Vec<Vec<u8>> {
        let mut maps = Vec::with_capacity(34);
        for level in 0..34u16 {
            let mut map = Vec::with_capacity(256);
            for i in 0..256u16 {
                // Darken towards black as level increases
                let factor = if level < 32 {
                    1.0 - (level as f32 / 32.0)
                } else {
                    0.0
                };
                let darkened = (i as f32 * factor) as u8;
                map.push(darkened);
            }
            maps.push(map);
        }
        maps
    }

    /// Look up a color by palette index.
    #[inline]
    pub fn color(&self, index: u8) -> [u8; 3] {
        self.colors[index as usize]
    }

    /// Look up a color with light level applied via colormap.
    /// `light_level` is 0-255 (sector light), `distance` is in map units.
    #[inline]
    pub fn lit_color(&self, index: u8, light_level: u8, distance: f32) -> [u8; 3] {
        let map_index = self.light_to_colormap(light_level, distance);
        if map_index < self.colormaps.len() {
            let mapped = self.colormaps[map_index][index as usize];
            self.colors[mapped as usize]
        } else {
            self.colors[index as usize]
        }
    }

    /// Convert sector light level + distance to a colormap index.
    /// This mimics Doom's light diminishing.
    #[inline]
    pub fn light_to_colormap(&self, light_level: u8, distance: f32) -> usize {
        // Doom divides light into 16 levels, then adjusts by distance
        let base_level = (light_level as f32 / 8.0) as i32; // 0-31
        let dist_factor = (distance / 128.0) as i32; // Distance attenuation
        (31 - base_level + dist_factor).clamp(0, 31) as usize
    }

    /// Get an RGB color for a sector light level and distance,
    /// applying a flat color (for walls without textures in Phase 1).
    pub fn wall_color(
        &self,
        base_r: u8,
        base_g: u8,
        base_b: u8,
        light: u8,
        distance: f32,
    ) -> [u8; 3] {
        let factor = self.light_factor(light, distance);
        [
            (base_r as f32 * factor) as u8,
            (base_g as f32 * factor) as u8,
            (base_b as f32 * factor) as u8,
        ]
    }

    /// Compute a 0.0-1.0 brightness factor from light level and distance.
    #[inline]
    pub fn light_factor(&self, light_level: u8, distance: f32) -> f32 {
        let base = light_level as f32 / 255.0;
        // Distance fog: gentle attenuation (1000 unit half-distance)
        let dist_atten = 1.0 / (1.0 + distance / 1000.0);
        (base * dist_atten).clamp(0.0, 1.0)
    }
}

impl Default for DoomPalette {
    fn default() -> Self {
        Self::default_palette()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_has_256_colors() {
        let pal = DoomPalette::default();
        assert_eq!(pal.colors.len(), 256);
    }

    #[test]
    fn default_colormaps_has_34_levels() {
        let pal = DoomPalette::default();
        assert_eq!(pal.colormaps.len(), 34);
    }

    #[test]
    fn light_factor_full_bright_close() {
        let pal = DoomPalette::default();
        let f = pal.light_factor(255, 0.0);
        assert!((f - 1.0).abs() < 0.01);
    }

    #[test]
    fn light_factor_diminishes_with_distance() {
        let pal = DoomPalette::default();
        let close = pal.light_factor(200, 100.0);
        let far = pal.light_factor(200, 1000.0);
        assert!(close > far);
    }

    #[test]
    fn color_lookup_returns_correct_rgb() {
        let pal = DoomPalette::default();
        // Default palette is grayscale: index N → [N, N, N]
        assert_eq!(pal.color(0), [0, 0, 0]);
        assert_eq!(pal.color(128), [128, 128, 128]);
        assert_eq!(pal.color(255), [255, 255, 255]);
    }

    #[test]
    fn from_wad_stores_custom_data() {
        let colors = vec![[10, 20, 30]; 256];
        let maps = vec![vec![0u8; 256]; 34];
        let pal = DoomPalette::from_wad(colors.clone(), maps.clone());
        assert_eq!(pal.colors.len(), 256);
        assert_eq!(pal.colormaps.len(), 34);
        assert_eq!(pal.color(0), [10, 20, 30]);
    }

    #[test]
    fn lit_color_at_full_light_close_distance() {
        let pal = DoomPalette::default();
        // Full light (255) at zero distance → colormap index should be ~0 (brightest)
        let color = pal.lit_color(128, 255, 0.0);
        // Colormap 0 maps index to itself (identity), so result should be close to [128,128,128]
        assert_eq!(color, [128, 128, 128]);
    }

    #[test]
    fn lit_color_darkens_at_far_distance() {
        let pal = DoomPalette::default();
        let close = pal.lit_color(200, 200, 0.0);
        let far = pal.lit_color(200, 200, 5000.0);
        // Farther away should be darker
        assert!(
            close[0] >= far[0],
            "close={close:?} should be brighter than far={far:?}"
        );
    }

    #[test]
    fn light_to_colormap_range_clamped() {
        let pal = DoomPalette::default();
        // Maximum light at zero distance → colormap 0
        let bright = pal.light_to_colormap(255, 0.0);
        assert_eq!(bright, 0);

        // Zero light at max distance → colormap 31
        let dark = pal.light_to_colormap(0, 100_000.0);
        assert_eq!(dark, 31);
    }

    #[test]
    fn wall_color_applies_light_and_distance() {
        let pal = DoomPalette::default();
        let close = pal.wall_color(200, 100, 50, 255, 0.0);
        let far = pal.wall_color(200, 100, 50, 255, 5000.0);
        // Closer should be brighter
        assert!(close[0] >= far[0]);
        assert!(close[1] >= far[1]);
    }

    #[test]
    fn light_factor_zero_light_is_zero() {
        let pal = DoomPalette::default();
        let f = pal.light_factor(0, 0.0);
        assert!(f.abs() < 0.01);
    }

    #[test]
    fn light_factor_is_clamped_to_unit_range() {
        let pal = DoomPalette::default();
        // Even with extreme values, factor stays in [0, 1]
        let f = pal.light_factor(255, 0.0);
        assert!((0.0..=1.0).contains(&f));
        let f2 = pal.light_factor(0, 100_000.0);
        assert!((0.0..=1.0).contains(&f2));
    }

    #[test]
    fn default_trait_matches_default_palette() {
        let a = DoomPalette::default();
        let b = DoomPalette::default_palette();
        assert_eq!(a.colors, b.colors);
        assert_eq!(a.colormaps.len(), b.colormaps.len());
    }

    #[test]
    fn colormap_brightest_is_identity() {
        let pal = DoomPalette::default();
        // Colormap 0 (brightest) should map each index to itself
        for i in 0..256u16 {
            assert_eq!(
                pal.colormaps[0][i as usize], i as u8,
                "colormap 0 should be identity at index {i}"
            );
        }
    }

    #[test]
    fn colormap_darkest_is_near_zero() {
        let pal = DoomPalette::default();
        // Colormap 31 (darkest regular) should map most indices towards 0
        for i in 0..256u16 {
            assert!(
                pal.colormaps[31][i as usize] <= (i as u8).saturating_add(1),
                "colormap 31 at {i} should be very dark"
            );
        }
    }

    #[test]
    fn special_colormaps_are_black() {
        let pal = DoomPalette::default();
        for level in [32usize, 33usize] {
            assert_eq!(pal.colormaps[level].len(), 256);
            assert!(
                pal.colormaps[level].iter().all(|&v| v == 0),
                "special colormap level {level} should map all indices to 0"
            );
        }
    }

    #[test]
    fn lit_color_falls_back_when_colormap_missing() {
        let colors = (0..=255u8).map(|i| [i, i, i]).collect::<Vec<_>>();
        let pal = DoomPalette::from_wad(colors, vec![]);
        assert_eq!(pal.lit_color(200, 255, 0.0), [200, 200, 200]);
    }

    #[test]
    fn each_colormap_has_256_entries() {
        let pal = DoomPalette::default();
        for (level, map) in pal.colormaps.iter().enumerate() {
            assert_eq!(map.len(), 256, "colormap {level} should have 256 entries");
        }
    }

    #[test]
    fn colormaps_monotonically_darken_with_level() {
        let pal = DoomPalette::default();
        for i in 1..256usize {
            for level in 1..32 {
                assert!(
                    pal.colormaps[level][i] <= pal.colormaps[level - 1][i],
                    "colormap[{level}][{i}]={} should be <= colormap[{}][{i}]={}",
                    pal.colormaps[level][i],
                    level - 1,
                    pal.colormaps[level - 1][i]
                );
            }
        }
    }

    #[test]
    fn light_to_colormap_increases_with_distance() {
        let pal = DoomPalette::default();
        let close = pal.light_to_colormap(128, 0.0);
        let mid = pal.light_to_colormap(128, 500.0);
        let far = pal.light_to_colormap(128, 5000.0);
        assert!(close <= mid, "close={close} should be <= mid={mid}");
        assert!(mid <= far, "mid={mid} should be <= far={far}");
    }

    #[test]
    fn light_to_colormap_decreases_with_light_level() {
        let pal = DoomPalette::default();
        let dark_sector = pal.light_to_colormap(50, 200.0);
        let bright_sector = pal.light_to_colormap(200, 200.0);
        assert!(
            bright_sector <= dark_sector,
            "brighter sector ({bright_sector}) should be <= darker ({dark_sector})"
        );
    }

    #[test]
    fn wall_color_zero_light_is_dark() {
        let pal = DoomPalette::default();
        let c = pal.wall_color(200, 150, 100, 0, 0.0);
        assert_eq!(c, [0, 0, 0]);
    }

    #[test]
    fn wall_color_full_light_zero_distance_near_base() {
        let pal = DoomPalette::default();
        let c = pal.wall_color(200, 150, 100, 255, 0.0);
        assert_eq!(c[0], 200);
        assert_eq!(c[1], 150);
        assert_eq!(c[2], 100);
    }

    #[test]
    fn from_wad_preserves_colormap_data() {
        let mut maps = vec![vec![0u8; 256]; 34];
        for (i, value) in maps[5].iter_mut().enumerate() {
            *value = (255 - i) as u8;
        }
        let pal = DoomPalette::from_wad(vec![[0, 0, 0]; 256], maps);
        assert_eq!(pal.colormaps[5][0], 255);
        assert_eq!(pal.colormaps[5][255], 0);
        assert_eq!(pal.colormaps[5][128], 127);
    }

    #[test]
    fn default_palette_is_grayscale() {
        let pal = DoomPalette::default();
        for i in 0u8..=u8::MAX {
            let c = pal.color(i);
            assert_eq!(c, [i, i, i], "index {i} should be grayscale");
        }
    }

    #[test]
    fn light_factor_half_distance() {
        let pal = DoomPalette::default();
        // At half-distance (1000): dist_atten = 1/(1+1) = 0.5
        // Full light: factor = (255/255) * 0.5 = 0.5
        let f = pal.light_factor(255, 1000.0);
        assert!((f - 0.5).abs() < 0.01, "expected ~0.5, got {f}");
    }

    #[test]
    fn light_to_colormap_mid_light_zero_distance() {
        let pal = DoomPalette::default();
        // light_level=128, distance=0: base_level = (128/8) as i32 = 16
        // result = (31 - 16 + 0).clamp(0, 31) = 15
        let idx = pal.light_to_colormap(128, 0.0);
        assert_eq!(idx, 15);
    }

    #[test]
    fn lit_color_with_mid_light_mid_distance() {
        let pal = DoomPalette::default();
        // Exercise lit_color through a realistic mid-range path
        let color = pal.lit_color(100, 128, 500.0);
        // Should produce a darkened version of palette entry
        assert!(color[0] < 100, "mid-light mid-distance should darken");
    }

    #[test]
    fn wall_color_channels_scale_independently() {
        let pal = DoomPalette::default();
        let c = pal.wall_color(200, 100, 50, 200, 500.0);
        // Each channel should be independently scaled by the same factor
        // factor = (200/255) * (1/(1+500/1000)) = ~0.784 * ~0.667 = ~0.523
        assert!(c[0] > c[1], "R should be greater than G");
        assert!(c[1] > c[2], "G should be greater than B");
    }

    #[test]
    fn generate_default_colormaps_special_levels_all_zero() {
        let pal = DoomPalette::default();
        // Levels 32 and 33 use factor=0.0, so all entries should be 0
        for level in 32..34 {
            for i in 0..256 {
                assert_eq!(
                    pal.colormaps[level][i], 0,
                    "special colormap[{level}][{i}] should be 0"
                );
            }
        }
    }

    #[test]
    fn generate_default_colormaps_mid_level_darkens_proportionally() {
        let pal = DoomPalette::default();
        // Level 16: factor = 1.0 - 16/32 = 0.5
        // Index 200: darkened = (200 * 0.5) as u8 = 100
        assert_eq!(pal.colormaps[16][200], 100);
    }

    #[test]
    fn light_to_colormap_extreme_distance_clamps_to_31() {
        let pal = DoomPalette::default();
        // Even with maximum light, extreme distance should clamp to 31
        let idx = pal.light_to_colormap(255, f32::MAX);
        assert_eq!(idx, 31);
    }

    #[test]
    fn light_to_colormap_all_light_levels_in_range() {
        let pal = DoomPalette::default();
        for light in 0..=255u8 {
            for dist in [0.0, 100.0, 500.0, 1000.0, 5000.0] {
                let idx = pal.light_to_colormap(light, dist);
                assert!(
                    idx <= 31,
                    "colormap index {idx} out of range for light={light} dist={dist}"
                );
            }
        }
    }

    #[test]
    fn light_factor_with_large_distance() {
        let pal = DoomPalette::default();
        // At very large distance, factor should approach 0
        let f = pal.light_factor(255, 1_000_000.0);
        assert!(f < 0.01, "expected near 0 at extreme distance, got {f}");
    }

    #[test]
    fn wall_color_mid_light_preserves_channel_ratio() {
        let pal = DoomPalette::default();
        let c = pal.wall_color(100, 200, 50, 128, 0.0);
        // factor = 128/255 ≈ 0.502
        // R = 100 * 0.502 ≈ 50, G = 200 * 0.502 ≈ 100, B = 50 * 0.502 ≈ 25
        assert!(c[1] > c[0], "G channel should be brightest");
        assert!(c[0] > c[2], "R channel should be brighter than B");
    }
}
