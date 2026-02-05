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

/// Sky gradient colors.
const SKY_TOP: [u8; 3] = [40, 60, 140];
const SKY_BOTTOM: [u8; 3] = [100, 140, 200];

/// Floor gradient colors.
const FLOOR_NEAR: [u8; 3] = [80, 70, 60];
const FLOOR_FAR: [u8; 3] = [40, 35, 30];

/// Ceiling color.
const CEILING_COLOR: [u8; 3] = [60, 60, 70];

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
        let _height = self.height;
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

                    // Draw the wall column
                    fb.draw_column(
                        x,
                        draw_top as u32,
                        draw_bottom as u32,
                        PackedRgba::rgb(r, g, b),
                    );

                    // Draw ceiling above wall (if not sky)
                    if draw_top > clip_top {
                        let ceil_light = light_factor * 0.7;
                        let cr = (CEILING_COLOR[0] as f32 * ceil_light) as u8;
                        let cg = (CEILING_COLOR[1] as f32 * ceil_light) as u8;
                        let cb = (CEILING_COLOR[2] as f32 * ceil_light) as u8;
                        if front.is_sky_ceiling() {
                            // Sky columns already drawn in background
                        } else {
                            fb.draw_column(
                                x,
                                clip_top as u32,
                                draw_top as u32,
                                PackedRgba::rgb(cr, cg, cb),
                            );
                        }
                    }

                    // Draw floor below wall
                    if draw_bottom < clip_bottom {
                        let floor_light = light_factor * 0.5;
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

                    // Update clipping
                    if is_solid {
                        column_clips[x as usize].solid = true;
                        *solid_count += 1;
                    } else {
                        // Two-sided: update clip range
                        if let Some(back) = back_sector {
                            let back_ceil = back.ceiling_height - player_view_z;
                            let back_floor = back.floor_height - player_view_z;

                            // Upper wall (if back ceiling is lower)
                            if back.ceiling_height < front.ceiling_height {
                                let upper_bottom =
                                    half_height - back_ceil * inv_depth + pitch_offset;
                                let ub = (upper_bottom as i32).max(clip_top).min(clip_bottom);

                                // Draw upper wall
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

                            // Lower wall (if back floor is higher)
                            if back.floor_height > front.floor_height {
                                let lower_top = half_height - back_floor * inv_depth + pitch_offset;
                                let lt = (lower_top as i32).max(clip_top).min(clip_bottom);

                                // Draw lower wall
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

            for x in 0..self.width {
                fb.set_pixel(x, y, color);
            }
        }
    }
}

/// Clip a line segment against the near plane (z > 0.1).
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
}
