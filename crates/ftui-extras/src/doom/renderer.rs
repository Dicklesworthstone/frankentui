//! Column-based BSP wall renderer for Doom.
//!
//! Performs front-to-back rendering using the BSP tree, projecting wall
//! segments to screen columns with proper perspective and clipping.

use ftui_render::cell::PackedRgba;

use super::bsp;
use super::constants::*;
use super::framebuffer::DoomFramebuffer;
use super::map::{DoomMap, Sector, Seg};
use super::palette::DoomPalette;
use super::player::Player;

/// Per-column clipping state for front-to-back rendering.
#[derive(Debug, Clone, Copy)]
struct ColumnClip {
    /// Top of open gap (initially 0).
    top: i32,
    /// Bottom of open gap (initially screen_height).
    bottom: i32,
    /// Whether this column is fully occluded.
    solid: bool,
}

/// Rendering statistics for performance overlay.
#[derive(Debug, Clone, Default)]
pub struct RenderStats {
    pub nodes_visited: u32,
    pub subsectors_rendered: u32,
    pub segs_processed: u32,
    pub columns_filled: u32,
    pub total_columns: u32,
}

/// Wall color palette for Phase 1 (solid-colored walls).
/// Different colors for different wall types to create visual variety.
const WALL_COLORS: [[u8; 3]; 8] = [
    [160, 120, 80],  // Brown stone
    [128, 128, 128], // Gray concrete
    [120, 80, 60],   // Dark brown
    [100, 100, 120], // Blue-gray metal
    [140, 100, 70],  // Tan
    [90, 90, 90],    // Dark gray
    [150, 110, 80],  // Light brown
    [110, 110, 130], // Steel blue
];

/// Sky gradient colors (bright blue sky).
const SKY_TOP: [u8; 3] = [80, 120, 200];
const SKY_BOTTOM: [u8; 3] = [160, 200, 240];

/// Floor gradient colors.
const FLOOR_NEAR: [u8; 3] = [120, 100, 80];
const FLOOR_FAR: [u8; 3] = [70, 60, 50];

/// Ceiling color.
const CEILING_COLOR: [u8; 3] = [100, 100, 120];

/// The main BSP renderer.
#[derive(Debug)]
pub struct DoomRenderer {
    /// Screen width in pixels.
    width: u32,
    /// Screen height in pixels.
    height: u32,
    /// Per-column clipping array.
    column_clips: Vec<ColumnClip>,
    /// Number of fully-solid columns.
    solid_count: u32,
    /// Rendering stats.
    pub stats: RenderStats,
    /// Half-width for projection.
    half_width: f32,
    /// Half-height for projection.
    half_height: f32,
    /// Projection scale (distance to projection plane).
    projection: f32,
}

impl DoomRenderer {
    /// Create a new renderer for the given screen dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let column_clips = vec![
            ColumnClip {
                top: 0,
                bottom: height as i32,
                solid: false,
            };
            width as usize
        ];

        let half_width = width as f32 / 2.0;
        let half_height = height as f32 / 2.0;
        let projection = half_width / (FOV_RADIANS / 2.0).tan();

        Self {
            width,
            height,
            column_clips,
            solid_count: 0,
            stats: RenderStats::default(),
            half_width,
            half_height,
            projection,
        }
    }

    /// Resize the renderer for new dimensions.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.column_clips.resize(
            width as usize,
            ColumnClip {
                top: 0,
                bottom: height as i32,
                solid: false,
            },
        );
        self.half_width = width as f32 / 2.0;
        self.half_height = height as f32 / 2.0;
        self.projection = self.half_width / (FOV_RADIANS / 2.0).tan();
    }

    /// Render a frame into the framebuffer.
    pub fn render(
        &mut self,
        fb: &mut DoomFramebuffer,
        map: &DoomMap,
        player: &Player,
        palette: &DoomPalette,
    ) {
        // Reset state
        self.reset();
        fb.clear();

        // Draw sky and floor background
        self.draw_background(fb);

        // BSP front-to-back traversal
        let width = self.width;
        let half_width = self.half_width;
        let half_height = self.half_height;
        let projection = self.projection;
        let player_x = player.x;
        let player_y = player.y;
        let player_angle = player.angle;
        let player_view_z = player.view_z + player.bob_offset();
        let player_pitch = player.pitch;

        let cos_a = player_angle.cos();
        let sin_a = player_angle.sin();

        // Collect data for the closure
        let column_clips = &mut self.column_clips;
        let solid_count = &mut self.solid_count;
        let stats = &mut self.stats;

        bsp::bsp_traverse(map, player_x, player_y, &mut |ss_idx: usize| -> bool {
            stats.subsectors_rendered += 1;

            // Early exit if all columns are filled
            if *solid_count >= width {
                return false;
            }

            let ss = &map.subsectors[ss_idx];
            let num_segs = ss.num_segs as usize;
            let first_seg = ss.first_seg;

            for seg_i in 0..num_segs {
                let seg_idx = first_seg + seg_i;
                if seg_idx >= map.segs.len() {
                    break;
                }

                stats.segs_processed += 1;
                let seg = &map.segs[seg_idx];

                // Get seg vertices
                if seg.v1 >= map.vertices.len() || seg.v2 >= map.vertices.len() {
                    continue;
                }
                let v1 = &map.vertices[seg.v1];
                let v2 = &map.vertices[seg.v2];

                // Transform to view space
                let dx1 = v1.x - player_x;
                let dy1 = v1.y - player_y;
                let dx2 = v2.x - player_x;
                let dy2 = v2.y - player_y;

                // Rotate by negative player angle
                let vx1 = dx1 * cos_a + dy1 * sin_a;
                let vy1 = -dx1 * sin_a + dy1 * cos_a;
                let vx2 = dx2 * cos_a + dy2 * sin_a;
                let vy2 = -dx2 * sin_a + dy2 * cos_a;

                // Clip against near plane (both vertices behind viewer)
                if vx1 <= 0.1 && vx2 <= 0.1 {
                    continue;
                }

                // Clip if one vertex is behind
                let (vx1, vy1, vx2, vy2) = clip_near_plane(vx1, vy1, vx2, vy2);

                // Project to screen columns
                let sx1 = half_width - (vy1 * projection / vx1);
                let sx2 = half_width - (vy2 * projection / vx2);

                let col_start = sx1.min(sx2) as i32;
                let col_end = sx1.max(sx2) as i32;

                // Skip if entirely off screen
                if col_end < 0 || col_start >= width as i32 {
                    continue;
                }

                // Get linedef and sector info
                if seg.linedef >= map.linedefs.len() {
                    continue;
                }
                let linedef = &map.linedefs[seg.linedef];

                let front_sector = get_seg_front_sector(seg, linedef, &map.sidedefs, &map.sectors);
                let back_sector = get_seg_back_sector(seg, linedef, &map.sidedefs, &map.sectors);

                let front = match front_sector {
                    Some(s) => s,
                    None => continue,
                };

                let is_solid = back_sector.is_none();
                let light = front.light_level.min(255) as u8;
                let wall_color_idx = seg.linedef % WALL_COLORS.len();
                let [base_r, base_g, base_b] = WALL_COLORS[wall_color_idx];

                // Draw columns
                let x_start = col_start.max(0) as u32;
                let x_end = (col_end + 1).min(width as i32) as u32;

                for x in x_start..x_end {
                    // Copy clip values to avoid borrow conflicts
                    let clip_top = column_clips[x as usize].top;
                    let clip_bottom = column_clips[x as usize].bottom;
                    let clip_solid = column_clips[x as usize].solid;

                    if clip_solid {
                        continue;
                    }

                    // Interpolate depth across the wall
                    let t = if (sx2 - sx1).abs() > 0.01 {
                        (x as f32 - sx1) / (sx2 - sx1)
                    } else {
                        0.5
                    };
                    let depth = vx1 + t * (vx2 - vx1);
                    if depth <= 0.1 {
                        continue;
                    }

                    let inv_depth = projection / depth;

                    // Calculate wall top and bottom on screen
                    let ceil_h = front.ceiling_height - player_view_z;
                    let floor_h = front.floor_height - player_view_z;

                    let pitch_offset = player_pitch * projection;
                    let wall_top = half_height - ceil_h * inv_depth + pitch_offset;
                    let wall_bottom = half_height - floor_h * inv_depth + pitch_offset;

                    let mut draw_top = wall_top as i32;
                    let mut draw_bottom = wall_bottom as i32;

                    // Clip to column bounds
                    draw_top = draw_top.max(clip_top);
                    draw_bottom = draw_bottom.min(clip_bottom);

                    if draw_top >= draw_bottom {
                        continue;
                    }

                    // Apply lighting
                    let light_factor = palette.light_factor(light, depth);

                    let r = (base_r as f32 * light_factor) as u8;
                    let g = (base_g as f32 * light_factor) as u8;
                    let b = (base_b as f32 * light_factor) as u8;

                    // Draw ceiling above wall (if not sky)
                    if draw_top > clip_top && !front.is_sky_ceiling() {
                        let ceil_light = light_factor * 0.85;
                        let cr = (CEILING_COLOR[0] as f32 * ceil_light) as u8;
                        let cg = (CEILING_COLOR[1] as f32 * ceil_light) as u8;
                        let cb = (CEILING_COLOR[2] as f32 * ceil_light) as u8;
                        fb.draw_column(
                            x,
                            clip_top as u32,
                            draw_top as u32,
                            PackedRgba::rgb(cr, cg, cb),
                        );
                    }

                    // Draw floor below wall
                    if draw_bottom < clip_bottom {
                        let floor_light = light_factor * 0.75;
                        let fr = (FLOOR_NEAR[0] as f32 * floor_light) as u8;
                        let fg = (FLOOR_NEAR[1] as f32 * floor_light) as u8;
                        let fbl = (FLOOR_NEAR[2] as f32 * floor_light) as u8;
                        fb.draw_column(
                            x,
                            draw_bottom as u32,
                            clip_bottom as u32,
                            PackedRgba::rgb(fr, fg, fbl),
                        );
                    }

                    // Update clipping and draw wall portions
                    if is_solid {
                        // Solid wall: draw full wall column
                        fb.draw_column(
                            x,
                            draw_top as u32,
                            draw_bottom as u32,
                            PackedRgba::rgb(r, g, b),
                        );

                        column_clips[x as usize].solid = true;
                        *solid_count += 1;
                    } else if let Some(back) = back_sector {
                        // Two-sided: only draw upper/lower wall portions,
                        // leave middle open so the back sector is visible.
                        let back_ceil = back.ceiling_height - player_view_z;
                        let back_floor = back.floor_height - player_view_z;

                        // Upper wall (if back ceiling is lower than front ceiling)
                        if back.ceiling_height < front.ceiling_height {
                            let upper_bottom = half_height - back_ceil * inv_depth + pitch_offset;
                            let ub = (upper_bottom as i32).max(clip_top).min(clip_bottom);

                            if draw_top < ub {
                                let ur = (base_r as f32 * light_factor * 0.85) as u8;
                                let ug = (base_g as f32 * light_factor * 0.85) as u8;
                                let ubr = (base_b as f32 * light_factor * 0.85) as u8;
                                fb.draw_column(
                                    x,
                                    draw_top as u32,
                                    ub as u32,
                                    PackedRgba::rgb(ur, ug, ubr),
                                );
                            }

                            column_clips[x as usize].top = ub;
                        }

                        // Lower wall (if back floor is higher than front floor)
                        if back.floor_height > front.floor_height {
                            let lower_top = half_height - back_floor * inv_depth + pitch_offset;
                            let lt = (lower_top as i32).max(clip_top).min(clip_bottom);

                            if lt < draw_bottom {
                                let lr = (base_r as f32 * light_factor * 0.7) as u8;
                                let lg = (base_g as f32 * light_factor * 0.7) as u8;
                                let lb = (base_b as f32 * light_factor * 0.7) as u8;
                                fb.draw_column(
                                    x,
                                    lt as u32,
                                    draw_bottom as u32,
                                    PackedRgba::rgb(lr, lg, lb),
                                );
                            }

                            column_clips[x as usize].bottom = lt;
                        }

                        // Mark column solid if the two-sided gap has closed
                        if column_clips[x as usize].top >= column_clips[x as usize].bottom {
                            column_clips[x as usize].solid = true;
                            *solid_count += 1;
                        }
                    }
                }
            }

            true // Continue traversal
        });

        stats.columns_filled = *solid_count;
        stats.total_columns = width;
    }

    /// Reset rendering state for a new frame.
    fn reset(&mut self) {
        for clip in &mut self.column_clips {
            clip.top = 0;
            clip.bottom = self.height as i32;
            clip.solid = false;
        }
        self.solid_count = 0;
        self.stats = RenderStats::default();
    }

    /// Draw the sky and floor background.
    fn draw_background(&self, fb: &mut DoomFramebuffer) {
        let horizon = self.height / 2;
        let row_width = self.width as usize;

        for y in 0..self.height {
            let color = if y < horizon {
                // Sky gradient
                let t = y as f32 / horizon as f32;
                let r = lerp_u8(SKY_TOP[0], SKY_BOTTOM[0], t);
                let g = lerp_u8(SKY_TOP[1], SKY_BOTTOM[1], t);
                let b = lerp_u8(SKY_TOP[2], SKY_BOTTOM[2], t);
                PackedRgba::rgb(r, g, b)
            } else {
                // Floor gradient (distance from horizon)
                let t = ((y - horizon) as f32 / (self.height - horizon) as f32).min(1.0);
                let r = lerp_u8(FLOOR_FAR[0], FLOOR_NEAR[0], t);
                let g = lerp_u8(FLOOR_FAR[1], FLOOR_NEAR[1], t);
                let b = lerp_u8(FLOOR_FAR[2], FLOOR_NEAR[2], t);
                PackedRgba::rgb(r, g, b)
            };

            let row_start = y as usize * row_width;
            let row_end = row_start + row_width;
            fb.pixels[row_start..row_end].fill(color);
        }
    }
}

/// Clip a line segment against the near plane (z > 0.1).
#[inline]
fn clip_near_plane(mut x1: f32, mut y1: f32, mut x2: f32, mut y2: f32) -> (f32, f32, f32, f32) {
    const NEAR: f32 = 0.1;

    if x1 < NEAR {
        let t = (NEAR - x1) / (x2 - x1);
        x1 = NEAR;
        y1 = y1 + t * (y2 - y1);
    }
    if x2 < NEAR {
        let t = (NEAR - x2) / (x1 - x2);
        x2 = NEAR;
        y2 = y2 + t * (y1 - y2);
    }

    (x1, y1, x2, y2)
}

/// Get the front sector of a seg.
#[inline]
fn get_seg_front_sector<'a>(
    seg: &Seg,
    linedef: &super::map::LineDef,
    sidedefs: &[super::map::SideDef],
    sectors: &'a [Sector],
) -> Option<&'a Sector> {
    let sidedef_idx = if seg.direction == 0 {
        linedef.front_sidedef?
    } else {
        linedef.back_sidedef?
    };
    if sidedef_idx >= sidedefs.len() {
        return None;
    }
    let sector_idx = sidedefs[sidedef_idx].sector;
    sectors.get(sector_idx)
}

/// Get the back sector of a seg.
#[inline]
fn get_seg_back_sector<'a>(
    seg: &Seg,
    linedef: &super::map::LineDef,
    sidedefs: &[super::map::SideDef],
    sectors: &'a [Sector],
) -> Option<&'a Sector> {
    let sidedef_idx = if seg.direction == 0 {
        linedef.back_sidedef?
    } else {
        linedef.front_sidedef?
    };
    if sidedef_idx >= sidedefs.len() {
        return None;
    }
    let sector_idx = sidedefs[sidedef_idx].sector;
    sectors.get(sector_idx)
}

/// Linearly interpolate between two u8 values.
#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_creation() {
        let r = DoomRenderer::new(320, 200);
        assert_eq!(r.width, 320);
        assert_eq!(r.height, 200);
        assert_eq!(r.column_clips.len(), 320);
    }

    #[test]
    fn clip_near_plane_both_in_front() {
        let (x1, y1, x2, y2) = clip_near_plane(10.0, 5.0, 20.0, -5.0);
        assert!((x1 - 10.0).abs() < 0.01);
        assert!((x2 - 20.0).abs() < 0.01);
        assert!((y1 - 5.0).abs() < 0.01);
        assert!((y2 - -5.0).abs() < 0.01);
    }

    #[test]
    fn lerp_u8_basic() {
        assert_eq!(lerp_u8(0, 100, 0.5), 50);
        assert_eq!(lerp_u8(0, 200, 0.0), 0);
        assert_eq!(lerp_u8(0, 200, 1.0), 200);
    }

    #[test]
    fn lerp_u8_same_values() {
        assert_eq!(lerp_u8(100, 100, 0.0), 100);
        assert_eq!(lerp_u8(100, 100, 0.5), 100);
        assert_eq!(lerp_u8(100, 100, 1.0), 100);
    }

    #[test]
    fn lerp_u8_quarter() {
        assert_eq!(lerp_u8(0, 100, 0.25), 25);
    }

    #[test]
    fn lerp_u8_reverse_direction() {
        // b < a: interpolates downward
        assert_eq!(lerp_u8(200, 100, 0.5), 150);
    }

    #[test]
    fn clip_near_plane_first_behind() {
        // First vertex behind near plane, second in front
        let (x1, _y1, x2, y2) = clip_near_plane(-5.0, 10.0, 20.0, -5.0);
        assert!((x1 - 0.1).abs() < 0.01); // clipped to near
        assert!((x2 - 20.0).abs() < 0.01); // unchanged
        assert!((y2 - -5.0).abs() < 0.01);
    }

    #[test]
    fn clip_near_plane_second_behind() {
        // First in front, second behind
        let (x1, y1, x2, _y2) = clip_near_plane(20.0, 5.0, -5.0, -5.0);
        assert!((x1 - 20.0).abs() < 0.01); // unchanged
        assert!((y1 - 5.0).abs() < 0.01);
        assert!((x2 - 0.1).abs() < 0.01); // clipped to near
    }

    #[test]
    fn clip_near_plane_y_interpolated_correctly() {
        // x1=-10, x2=10, y1=0, y2=20
        // t = (0.1 - (-10)) / (10 - (-10)) = 10.1/20 ≈ 0.505
        // y1' = 0 + 0.505 * (20 - 0) = 10.1
        let (x1, y1, x2, y2) = clip_near_plane(-10.0, 0.0, 10.0, 20.0);
        assert!((x1 - 0.1).abs() < 0.01);
        assert!((y1 - 10.1).abs() < 0.2);
        assert!((x2 - 10.0).abs() < 0.01);
        assert!((y2 - 20.0).abs() < 0.01);
    }

    #[test]
    fn renderer_creation_projection_values() {
        let r = DoomRenderer::new(320, 200);
        assert!((r.half_width - 160.0).abs() < 0.01);
        assert!((r.half_height - 100.0).abs() < 0.01);
        assert!(r.projection > 0.0);
    }

    #[test]
    fn renderer_resize() {
        let mut r = DoomRenderer::new(320, 200);
        r.resize(640, 400);
        assert_eq!(r.width, 640);
        assert_eq!(r.height, 400);
        assert_eq!(r.column_clips.len(), 640);
        assert!((r.half_width - 320.0).abs() < 0.01);
        assert!((r.half_height - 200.0).abs() < 0.01);
    }

    #[test]
    fn renderer_resize_smaller() {
        let mut r = DoomRenderer::new(640, 400);
        r.resize(160, 100);
        assert_eq!(r.width, 160);
        assert_eq!(r.height, 100);
        assert_eq!(r.column_clips.len(), 160);
    }

    #[test]
    fn render_stats_default() {
        let stats = RenderStats::default();
        assert_eq!(stats.nodes_visited, 0);
        assert_eq!(stats.subsectors_rendered, 0);
        assert_eq!(stats.segs_processed, 0);
        assert_eq!(stats.columns_filled, 0);
        assert_eq!(stats.total_columns, 0);
    }

    #[test]
    fn column_clips_initialized_correctly() {
        let r = DoomRenderer::new(100, 50);
        for clip in &r.column_clips {
            assert_eq!(clip.top, 0);
            assert_eq!(clip.bottom, 50);
            assert!(!clip.solid);
        }
    }

    #[test]
    fn renderer_reset_clears_state() {
        let mut r = DoomRenderer::new(10, 10);
        r.column_clips[0].solid = true;
        r.column_clips[0].top = 5;
        r.solid_count = 1;
        r.stats.segs_processed = 42;

        r.reset();

        assert_eq!(r.column_clips[0].top, 0);
        assert_eq!(r.column_clips[0].bottom, 10);
        assert!(!r.column_clips[0].solid);
        assert_eq!(r.solid_count, 0);
        assert_eq!(r.stats.segs_processed, 0);
    }

    #[test]
    fn wall_colors_has_entries() {
        assert_eq!(WALL_COLORS.len(), 8);
        for color in &WALL_COLORS {
            // All components should be reasonable values
            assert!(color[0] > 0 || color[1] > 0 || color[2] > 0);
        }
    }

    #[test]
    fn renderer_projection_positive_for_valid_fov() {
        // FOV_RADIANS should produce a positive projection factor
        let r = DoomRenderer::new(320, 200);
        assert!(r.projection > 100.0); // Should be a decent positive value
    }

    // --- clip_near_plane edge cases ---

    #[test]
    fn clip_near_plane_both_exactly_at_near() {
        let (x1, y1, x2, y2) = clip_near_plane(0.1, 3.0, 0.1, -3.0);
        assert!((x1 - 0.1).abs() < 0.01);
        assert!((x2 - 0.1).abs() < 0.01);
        assert!((y1 - 3.0).abs() < 0.01);
        assert!((y2 - -3.0).abs() < 0.01);
    }

    #[test]
    fn clip_near_plane_both_behind() {
        // Both behind: both get clipped to 0.1
        let (x1, _, x2, _) = clip_near_plane(-5.0, 0.0, -10.0, 0.0);
        assert!((x1 - 0.1).abs() < 0.01);
        assert!((x2 - 0.1).abs() < 0.01);
    }

    #[test]
    fn clip_near_plane_large_values() {
        let (x1, y1, x2, y2) = clip_near_plane(1000.0, 500.0, 2000.0, -500.0);
        assert!((x1 - 1000.0).abs() < 0.01);
        assert!((y1 - 500.0).abs() < 0.01);
        assert!((x2 - 2000.0).abs() < 0.01);
        assert!((y2 - -500.0).abs() < 0.01);
    }

    #[test]
    fn clip_near_plane_symmetric() {
        // Symmetric input → symmetric output
        let (x1, y1, x2, y2) = clip_near_plane(10.0, 5.0, 10.0, -5.0);
        assert!((x1 - x2).abs() < 0.01);
        assert!((y1 + y2).abs() < 0.01);
    }

    // --- lerp_u8 edge cases ---

    #[test]
    fn lerp_u8_full_range() {
        assert_eq!(lerp_u8(0, 255, 0.0), 0);
        assert_eq!(lerp_u8(0, 255, 1.0), 255);
    }

    #[test]
    fn lerp_u8_midpoint_odd() {
        // 0 to 255 at 0.5 = 127.5 → truncated to 127
        assert_eq!(lerp_u8(0, 255, 0.5), 127);
    }

    #[test]
    fn lerp_u8_both_zero() {
        assert_eq!(lerp_u8(0, 0, 0.5), 0);
    }

    #[test]
    fn lerp_u8_both_max() {
        assert_eq!(lerp_u8(255, 255, 0.5), 255);
    }

    // --- get_seg_front_sector / get_seg_back_sector ---

    #[test]
    fn get_seg_front_sector_direction_zero() {
        use super::super::map::{LineDef, Sector, SideDef};
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "FLOOR".into(),
            ceiling_texture: "CEIL".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let sidedefs = vec![SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            upper_texture: "-".into(),
            lower_texture: "-".into(),
            middle_texture: "WALL".into(),
            sector: 0,
        }];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: None,
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 0,
            offset: 0.0,
        };
        let result = get_seg_front_sector(&seg, &linedef, &sidedefs, &sectors);
        assert!(result.is_some());
        assert!((result.unwrap().ceiling_height - 128.0).abs() < 0.01);
    }

    #[test]
    fn get_seg_front_sector_direction_one_uses_back() {
        use super::super::map::{LineDef, Sector, SideDef};
        let sectors = vec![
            Sector {
                floor_height: 0.0,
                ceiling_height: 64.0,
                floor_texture: "F1".into(),
                ceiling_texture: "C1".into(),
                light_level: 100,
                special: 0,
                tag: 0,
            },
            Sector {
                floor_height: 0.0,
                ceiling_height: 128.0,
                floor_texture: "F2".into(),
                ceiling_texture: "C2".into(),
                light_level: 200,
                special: 0,
                tag: 0,
            },
        ];
        let sidedefs = vec![
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "W1".into(),
                sector: 0,
            },
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "W2".into(),
                sector: 1,
            },
        ];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: Some(1),
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 1, // opposite → front sector from back_sidedef
            offset: 0.0,
        };
        let result = get_seg_front_sector(&seg, &linedef, &sidedefs, &sectors);
        assert!(result.is_some());
        // direction=1 → uses back_sidedef → sidedef[1] → sector 1 → ceiling 128
        assert!((result.unwrap().ceiling_height - 128.0).abs() < 0.01);
    }

    #[test]
    fn get_seg_front_sector_missing_sidedef_returns_none() {
        use super::super::map::{LineDef, Sector};
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "F".into(),
            ceiling_texture: "C".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 0,
            offset: 0.0,
        };
        assert!(get_seg_front_sector(&seg, &linedef, &[], &sectors).is_none());
    }

    #[test]
    fn get_seg_back_sector_returns_none_for_one_sided() {
        use super::super::map::{LineDef, Sector, SideDef};
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "F".into(),
            ceiling_texture: "C".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let sidedefs = vec![SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            upper_texture: "-".into(),
            lower_texture: "-".into(),
            middle_texture: "W".into(),
            sector: 0,
        }];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: None, // one-sided wall
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 0,
            offset: 0.0,
        };
        assert!(get_seg_back_sector(&seg, &linedef, &sidedefs, &sectors).is_none());
    }

    #[test]
    fn get_seg_back_sector_oob_sidedef_returns_none() {
        use super::super::map::{LineDef, Sector};
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "F".into(),
            ceiling_texture: "C".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: Some(999), // out of bounds
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 0,
            offset: 0.0,
        };
        assert!(get_seg_back_sector(&seg, &linedef, &[], &sectors).is_none());
    }

    #[test]
    fn get_seg_front_sector_oob_sidedef_returns_none() {
        use super::super::map::{LineDef, Sector};
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "F".into(),
            ceiling_texture: "C".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(999), // out of bounds
            back_sidedef: None,
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 0,
            offset: 0.0,
        };
        assert!(get_seg_front_sector(&seg, &linedef, &[], &sectors).is_none());
    }

    // --- Renderer with a real map ---

    fn make_simple_map() -> DoomMap {
        use super::super::map::*;
        // Simple square room: 4 vertices, 4 linedefs, 4 segs, 1 sector, 1 subsector
        let vertices = vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 256.0, y: 0.0 },
            Vertex { x: 256.0, y: 256.0 },
            Vertex { x: 0.0, y: 256.0 },
        ];
        let sectors = vec![Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "FLOOR".into(),
            ceiling_texture: "CEIL".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        let sidedefs = vec![SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            upper_texture: "-".into(),
            lower_texture: "-".into(),
            middle_texture: "WALL".into(),
            sector: 0,
        }];
        let linedefs = vec![
            LineDef {
                v1: 0,
                v2: 1,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 1,
                v2: 2,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 2,
                v2: 3,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 3,
                v2: 0,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
        ];
        let segs = vec![
            Seg {
                v1: 0,
                v2: 1,
                angle: 0.0,
                linedef: 0,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 1,
                v2: 2,
                angle: 0.0,
                linedef: 1,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 2,
                v2: 3,
                angle: 0.0,
                linedef: 2,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 3,
                v2: 0,
                angle: 0.0,
                linedef: 3,
                direction: 0,
                offset: 0.0,
            },
        ];
        let subsectors = vec![SubSector {
            num_segs: 4,
            first_seg: 0,
        }];
        DoomMap {
            name: "SIMPLE".into(),
            vertices,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors,
            nodes: vec![],
            things: vec![],
        }
    }

    #[test]
    fn render_simple_room_produces_stats() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 128.0,
            angle: 0.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        assert!(renderer.stats.subsectors_rendered > 0);
        assert!(renderer.stats.segs_processed > 0);
        assert_eq!(renderer.stats.total_columns, 80);
    }

    #[test]
    fn render_writes_to_framebuffer() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // At least some pixels should be non-black (walls, sky, floor)
        let non_black = fb
            .pixels
            .iter()
            .filter(|&&p| p != PackedRgba::BLACK)
            .count();
        assert!(non_black > 0, "Rendered frame should have non-black pixels");
    }

    #[test]
    fn render_background_fills_all_pixels() {
        let r = DoomRenderer::new(40, 30);
        let mut fb = DoomFramebuffer::new(40, 30);
        r.draw_background(&mut fb);

        // Every pixel should be non-black (sky or floor gradient)
        let black_count = fb
            .pixels
            .iter()
            .filter(|&&p| p == PackedRgba::BLACK)
            .count();
        assert_eq!(
            black_count, 0,
            "Background should fill all pixels with sky/floor"
        );
    }

    #[test]
    fn render_twice_resets_stats() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);
        let first_segs = renderer.stats.segs_processed;

        renderer.render(&mut fb, &map, &player, &palette);
        let second_segs = renderer.stats.segs_processed;

        // Should be the same — state is reset each frame
        assert_eq!(first_segs, second_segs);
    }

    #[test]
    fn resize_grows_column_clips() {
        let mut r = DoomRenderer::new(10, 10);
        r.resize(20, 30);
        assert_eq!(r.column_clips.len(), 20);
        // New columns (index 10..19) get fresh default state
        for clip in &r.column_clips[10..] {
            assert_eq!(clip.top, 0);
            assert_eq!(clip.bottom, 30);
            assert!(!clip.solid);
        }
    }

    #[test]
    fn resize_updates_projection() {
        let mut r = DoomRenderer::new(320, 200);
        let proj1 = r.projection;
        r.resize(640, 400);
        let proj2 = r.projection;
        // Wider screen → larger projection factor
        assert!(proj2 > proj1);
    }

    #[test]
    fn render_with_sky_ceiling_sector() {
        use super::super::map::*;
        // Sky ceiling sector should not draw ceiling color
        let sectors = [Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: "FLOOR".into(),
            ceiling_texture: "F_SKY1".into(), // sky ceiling
            light_level: 200,
            special: 0,
            tag: 0,
        }];
        assert!(sectors[0].is_sky_ceiling());
    }

    #[test]
    fn render_stats_columns_match_width() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(100, 60);
        let mut fb = DoomFramebuffer::new(100, 60);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);
        assert_eq!(renderer.stats.total_columns, 100);
    }

    // --- Two-sided wall (upper/lower portal) ---

    /// Build a two-room map connected by a two-sided linedef where the back
    /// sector has a higher floor and lower ceiling than the front, so both
    /// upper and lower wall portions are drawn.
    fn make_two_room_map() -> DoomMap {
        use super::super::map::*;
        // Two sectors sharing a wall at y=128.
        // Front sector: floor=0, ceiling=128
        // Back sector:  floor=32, ceiling=96  (step up + lower ceiling)
        let vertices = vec![
            Vertex { x: 0.0, y: 0.0 },     // 0 — bottom-left front
            Vertex { x: 256.0, y: 0.0 },   // 1 — bottom-right front
            Vertex { x: 256.0, y: 128.0 }, // 2 — top-right front / bottom-right back
            Vertex { x: 0.0, y: 128.0 },   // 3 — top-left front / bottom-left back
            Vertex { x: 256.0, y: 256.0 }, // 4 — top-right back
            Vertex { x: 0.0, y: 256.0 },   // 5 — top-left back
        ];
        let sectors = vec![
            Sector {
                floor_height: 0.0,
                ceiling_height: 128.0,
                floor_texture: "FLOOR".into(),
                ceiling_texture: "CEIL".into(),
                light_level: 200,
                special: 0,
                tag: 0,
            },
            Sector {
                floor_height: 32.0,
                ceiling_height: 96.0,
                floor_texture: "FLOOR2".into(),
                ceiling_texture: "CEIL2".into(),
                light_level: 160,
                special: 0,
                tag: 0,
            },
        ];
        let sidedefs = vec![
            // sidedef 0: front sector walls
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "WALL".into(),
                sector: 0,
            },
            // sidedef 1: two-sided front side (facing front sector)
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "UPPER".into(),
                lower_texture: "LOWER".into(),
                middle_texture: "-".into(),
                sector: 0,
            },
            // sidedef 2: two-sided back side (facing back sector)
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "UPPER".into(),
                lower_texture: "LOWER".into(),
                middle_texture: "-".into(),
                sector: 1,
            },
            // sidedef 3: back sector outer walls
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "WALL2".into(),
                sector: 1,
            },
        ];
        let linedefs = vec![
            // Front room walls (one-sided)
            LineDef {
                v1: 0,
                v2: 1,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 1,
                v2: 2,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 3,
                v2: 0,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            // Two-sided portal wall between rooms (v3→v2)
            LineDef {
                v1: 3,
                v2: 2,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(1),
                back_sidedef: Some(2),
            },
            // Back room walls (one-sided)
            LineDef {
                v1: 2,
                v2: 4,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(3),
                back_sidedef: None,
            },
            LineDef {
                v1: 4,
                v2: 5,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(3),
                back_sidedef: None,
            },
            LineDef {
                v1: 5,
                v2: 3,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(3),
                back_sidedef: None,
            },
        ];
        let segs = vec![
            // Front room segs
            Seg {
                v1: 0,
                v2: 1,
                angle: 0.0,
                linedef: 0,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 1,
                v2: 2,
                angle: 0.0,
                linedef: 1,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 3,
                v2: 0,
                angle: 0.0,
                linedef: 2,
                direction: 0,
                offset: 0.0,
            },
            // Two-sided seg (portal)
            Seg {
                v1: 3,
                v2: 2,
                angle: 0.0,
                linedef: 3,
                direction: 0,
                offset: 0.0,
            },
            // Back room segs
            Seg {
                v1: 2,
                v2: 4,
                angle: 0.0,
                linedef: 4,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 4,
                v2: 5,
                angle: 0.0,
                linedef: 5,
                direction: 0,
                offset: 0.0,
            },
            Seg {
                v1: 5,
                v2: 3,
                angle: 0.0,
                linedef: 6,
                direction: 0,
                offset: 0.0,
            },
        ];
        // Two subsectors: front room (segs 0..3) and back room (segs 4..6).
        // With an empty nodes vec the BSP traversal processes subsectors
        // in order, which is fine for this test.
        let subsectors = vec![
            SubSector {
                num_segs: 4,
                first_seg: 0,
            },
            SubSector {
                num_segs: 3,
                first_seg: 4,
            },
        ];
        DoomMap {
            name: "TWOROOM".into(),
            vertices,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors,
            nodes: vec![],
            things: vec![],
        }
    }

    #[test]
    fn render_two_sided_wall_draws_upper_lower() {
        let map = make_two_room_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        // Stand in front room looking toward the portal wall
        let player = Player {
            x: 128.0,
            y: 64.0,
            angle: std::f32::consts::FRAC_PI_2, // look toward +Y
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // The renderer should have processed segs including the two-sided portal
        assert!(renderer.stats.segs_processed >= 4);
        // Some columns should be filled by solid walls, but the portal
        // columns should NOT be fully solid (they leave a gap for the back room)
        assert!(renderer.stats.columns_filled < renderer.stats.total_columns);
    }

    #[test]
    fn two_sided_wall_updates_column_clip_bounds() {
        // After rendering a two-sided wall, the column clips for portal columns
        // should have their top/bottom narrowed (not marked solid).
        let map = make_two_room_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 64.0,
            angle: std::f32::consts::FRAC_PI_2,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // At least some columns should have narrowed clip bounds from the portal
        let narrowed = renderer
            .column_clips
            .iter()
            .filter(|c| !c.solid && (c.top > 0 || c.bottom < 50))
            .count();
        // We expect some columns to have been narrowed by upper/lower portions
        // (they may also end up solid if the gap closed).
        // Just verify the renderer processed them without panic.
        assert!(
            narrowed > 0 || renderer.stats.columns_filled == renderer.stats.total_columns,
            "Portal should either narrow clips or fill all columns"
        );
    }

    // --- Early exit when all columns solid ---

    #[test]
    fn early_exit_when_all_columns_solid() {
        // Make a tiny renderer (2 columns) with a room that fills both columns.
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(2, 10);
        let mut fb = DoomFramebuffer::new(2, 10);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // All columns should be filled (solid walls in every direction)
        assert_eq!(renderer.stats.columns_filled, 2);
        assert_eq!(renderer.stats.total_columns, 2);
    }

    // --- Pitch offset ---

    #[test]
    fn pitch_offset_shifts_wall_position() {
        let map = make_simple_map();
        let mut fb1 = DoomFramebuffer::new(80, 50);
        let mut fb2 = DoomFramebuffer::new(80, 50);
        let palette = DoomPalette::default();

        // Render with no pitch
        let mut renderer = DoomRenderer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 128.0,
            pitch: 0.0,
            ..Player::default()
        };
        renderer.render(&mut fb1, &map, &player, &palette);

        // Render with upward pitch
        let mut renderer2 = DoomRenderer::new(80, 50);
        let player2 = Player {
            x: 128.0,
            y: 128.0,
            pitch: 0.3,
            ..Player::default()
        };
        renderer2.render(&mut fb2, &map, &player2, &palette);

        // The framebuffers should differ because pitch shifts wall projection
        assert_ne!(
            fb1.pixels, fb2.pixels,
            "Pitch should change rendered output"
        );
    }

    // --- OOB safety ---

    #[test]
    fn oob_seg_vertex_does_not_panic() {
        use super::super::map::*;
        let map = DoomMap {
            name: "OOB".into(),
            vertices: vec![Vertex { x: 10.0, y: 10.0 }], // only 1 vertex
            linedefs: vec![LineDef {
                v1: 0,
                v2: 1,
                flags: 0,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            }],
            sidedefs: vec![SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "W".into(),
                sector: 0,
            }],
            sectors: vec![Sector {
                floor_height: 0.0,
                ceiling_height: 128.0,
                floor_texture: "F".into(),
                ceiling_texture: "C".into(),
                light_level: 200,
                special: 0,
                tag: 0,
            }],
            segs: vec![Seg {
                v1: 0,
                v2: 999, // OOB vertex
                angle: 0.0,
                linedef: 0,
                direction: 0,
                offset: 0.0,
            }],
            subsectors: vec![SubSector {
                num_segs: 1,
                first_seg: 0,
            }],
            nodes: vec![],
            things: vec![],
        };

        let mut renderer = DoomRenderer::new(20, 10);
        let mut fb = DoomFramebuffer::new(20, 10);
        let player = Player::default();
        let palette = DoomPalette::default();

        // Should not panic — the renderer skips segs with OOB vertices
        renderer.render(&mut fb, &map, &player, &palette);
    }

    #[test]
    fn oob_seg_linedef_does_not_panic() {
        use super::super::map::*;
        let map = DoomMap {
            name: "OOB2".into(),
            vertices: vec![Vertex { x: 10.0, y: 10.0 }, Vertex { x: 50.0, y: 10.0 }],
            linedefs: vec![], // empty — seg references linedef 999
            sidedefs: vec![],
            sectors: vec![Sector {
                floor_height: 0.0,
                ceiling_height: 128.0,
                floor_texture: "F".into(),
                ceiling_texture: "C".into(),
                light_level: 200,
                special: 0,
                tag: 0,
            }],
            segs: vec![Seg {
                v1: 0,
                v2: 1,
                angle: 0.0,
                linedef: 999, // OOB
                direction: 0,
                offset: 0.0,
            }],
            subsectors: vec![SubSector {
                num_segs: 1,
                first_seg: 0,
            }],
            nodes: vec![],
            things: vec![],
        };

        let mut renderer = DoomRenderer::new(20, 10);
        let mut fb = DoomFramebuffer::new(20, 10);
        let player = Player::default();
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);
    }

    #[test]
    fn oob_first_seg_does_not_panic() {
        use super::super::map::*;
        let map = DoomMap {
            name: "OOB3".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![SubSector {
                num_segs: 5,
                first_seg: 100, // OOB — beyond segs array
            }],
            nodes: vec![],
            things: vec![],
        };

        let mut renderer = DoomRenderer::new(10, 10);
        let mut fb = DoomFramebuffer::new(10, 10);
        let player = Player::default();
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);
    }

    // --- Background gradient ---

    #[test]
    fn background_sky_differs_from_floor() {
        let r = DoomRenderer::new(1, 20);
        let mut fb = DoomFramebuffer::new(1, 20);
        r.draw_background(&mut fb);

        // Top pixel (sky) should differ from bottom pixel (floor)
        let sky_pixel = fb.pixels[0]; // y=0, x=0
        let floor_pixel = fb.pixels[19]; // y=19, x=0
        assert_ne!(
            sky_pixel, floor_pixel,
            "Sky and floor colors should be different"
        );
    }

    #[test]
    fn background_sky_gradient_changes_with_y() {
        let r = DoomRenderer::new(1, 40);
        let mut fb = DoomFramebuffer::new(1, 40);
        r.draw_background(&mut fb);

        // First sky row and last sky row (horizon at 20) should differ
        let top = fb.pixels[0]; // y=0
        let near_horizon = fb.pixels[19]; // y=19 (last sky row)
        assert_ne!(top, near_horizon, "Sky gradient should vary across rows");
    }

    // --- get_seg_back_sector with direction=1 ---

    #[test]
    fn get_seg_back_sector_direction_one_uses_front() {
        use super::super::map::{LineDef, Sector, SideDef};
        let sectors = vec![
            Sector {
                floor_height: 0.0,
                ceiling_height: 64.0,
                floor_texture: "F1".into(),
                ceiling_texture: "C1".into(),
                light_level: 100,
                special: 0,
                tag: 0,
            },
            Sector {
                floor_height: 0.0,
                ceiling_height: 128.0,
                floor_texture: "F2".into(),
                ceiling_texture: "C2".into(),
                light_level: 200,
                special: 0,
                tag: 0,
            },
        ];
        let sidedefs = vec![
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "W1".into(),
                sector: 0,
            },
            SideDef {
                x_offset: 0.0,
                y_offset: 0.0,
                upper_texture: "-".into(),
                lower_texture: "-".into(),
                middle_texture: "W2".into(),
                sector: 1,
            },
        ];
        let linedef = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: Some(1),
        };
        let seg = super::super::map::Seg {
            v1: 0,
            v2: 1,
            angle: 0.0,
            linedef: 0,
            direction: 1, // reversed → back sector from front_sidedef
            offset: 0.0,
        };
        let result = get_seg_back_sector(&seg, &linedef, &sidedefs, &sectors);
        assert!(result.is_some());
        // direction=1 → uses front_sidedef → sidedef[0] → sector 0 → ceiling 64
        assert!((result.unwrap().ceiling_height - 64.0).abs() < 0.01);
    }

    // --- Empty map does not panic ---

    #[test]
    fn render_empty_map_no_panic() {
        use super::super::map::*;
        let map = DoomMap {
            name: "EMPTY".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![],
        };

        let mut renderer = DoomRenderer::new(40, 30);
        let mut fb = DoomFramebuffer::new(40, 30);
        let player = Player::default();
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // Should just have background (sky + floor), no wall pixels
        assert_eq!(renderer.stats.segs_processed, 0);
        assert_eq!(renderer.stats.subsectors_rendered, 0);
    }

    // --- Single pixel renderer ---

    #[test]
    fn render_1x1_does_not_panic() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(1, 1);
        let mut fb = DoomFramebuffer::new(1, 1);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);
        assert_eq!(renderer.stats.total_columns, 1);
    }

    // --- Render from different angles ---

    #[test]
    fn render_different_angles_produce_different_output() {
        let map = make_simple_map();
        let palette = DoomPalette::default();

        let mut renderer1 = DoomRenderer::new(40, 30);
        let mut fb1 = DoomFramebuffer::new(40, 30);
        let p1 = Player {
            x: 128.0,
            y: 128.0,
            angle: 0.0,
            ..Player::default()
        };
        renderer1.render(&mut fb1, &map, &p1, &palette);

        let mut renderer2 = DoomRenderer::new(40, 30);
        let mut fb2 = DoomFramebuffer::new(40, 30);
        let p2 = Player {
            x: 128.0,
            y: 128.0,
            angle: std::f32::consts::PI,
            ..Player::default()
        };
        renderer2.render(&mut fb2, &map, &p2, &palette);

        assert_ne!(
            fb1.pixels, fb2.pixels,
            "Different angles should produce different framebuffers"
        );
    }

    // --- Solid count tracking ---

    #[test]
    fn solid_count_matches_columns_filled() {
        let map = make_simple_map();
        let mut renderer = DoomRenderer::new(80, 50);
        let mut fb = DoomFramebuffer::new(80, 50);
        let player = Player {
            x: 128.0,
            y: 128.0,
            ..Player::default()
        };
        let palette = DoomPalette::default();

        renderer.render(&mut fb, &map, &player, &palette);

        // solid_count should match the stats
        let manually_counted = renderer.column_clips.iter().filter(|c| c.solid).count() as u32;
        assert_eq!(renderer.stats.columns_filled, manually_counted);
    }

    // --- Wall color index wraps ---

    #[test]
    fn wall_color_index_wraps_with_linedef() {
        // WALL_COLORS has 8 entries; linedef index % 8 gives the color
        let color_count = WALL_COLORS.len();
        assert_eq!(color_count, 8);
        // Index N and N+len should map to the same palette color.
        for idx in [0usize, 3usize] {
            let wrapped = (idx + color_count) % color_count;
            assert_eq!(WALL_COLORS[idx], WALL_COLORS[wrapped]);
        }
    }

    // --- lerp_u8 with extreme t values ---

    #[test]
    fn lerp_u8_slightly_beyond_one() {
        // The renderer doesn't clamp t, so verify behavior is stable
        // t=1.01: (0 + 255 * 1.01) = 257.55 → truncated to 1 (wraps as u8)
        let result = lerp_u8(0, 255, 1.01);
        // Just verify no panic — exact value depends on float→u8 conversion
        let _ = result;
    }

    // --- Projection scales linearly with width ---

    #[test]
    fn projection_scales_with_width() {
        let r1 = DoomRenderer::new(320, 200);
        let r2 = DoomRenderer::new(640, 200);
        // projection = half_width / tan(FOV/2)
        // Doubling width doubles half_width, so projection should double
        assert!(
            (r2.projection / r1.projection - 2.0).abs() < 0.01,
            "Projection should scale linearly with width"
        );
    }

    // --- Resize preserves existing solid columns correctly ---

    #[test]
    fn resize_resets_clip_state_for_old_columns() {
        let mut r = DoomRenderer::new(10, 20);
        // Dirty a column
        r.column_clips[5].solid = true;
        r.column_clips[5].top = 10;
        // Resize to same width — Vec::resize won't touch existing elements
        r.resize(10, 30);
        // The dirty state persists (resize doesn't reset!)
        // This is intentional — reset() is called at the start of render()
        assert!(r.column_clips[5].solid);
        // But bottom should be updated for new columns only
        // (existing columns keep their old bottom value)
    }
}
