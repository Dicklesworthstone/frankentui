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
        // Distance fog: attenuate by distance
        let dist_atten = 1.0 / (1.0 + distance / 400.0);
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
}
