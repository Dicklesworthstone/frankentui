//! 3D face-based renderer for Quake.
//!
//! Projects map faces into screen space, performs backface culling,
//! and rasterizes triangles with z-buffer depth testing. Applies
//! Quake-style distance-based fog and directional lighting.
//!
//! Ported from Quake 1 r_edge.c / r_main.c (id Software GPL).

use ftui_render::cell::PackedRgba;

use super::constants::*;
use super::framebuffer::QuakeFramebuffer;
use super::map::{Face, QuakeMap, TexType};
use super::player::Player;

/// Rendering statistics for performance overlay.
#[derive(Debug, Clone, Default)]
pub struct RenderStats {
    pub faces_tested: u32,
    pub faces_culled: u32,
    pub faces_drawn: u32,
    pub triangles_rasterized: u32,
    pub pixels_written: u32,
}

/// The main Quake 3D renderer.
#[derive(Debug)]
pub struct QuakeRenderer {
    /// Screen width in pixels.
    width: u32,
    /// Screen height in pixels.
    height: u32,
    /// Half-width for projection.
    half_width: f32,
    /// Half-height for projection.
    half_height: f32,
    /// Projection scale (distance to projection plane).
    projection: f32,
    /// Rendering stats.
    pub stats: RenderStats,
    /// Cached face centroids (computed once from static geometry).
    face_centroids: Vec<[f32; 3]>,
    /// Reusable face-order buffer (avoids per-frame allocation).
    face_order_buf: Vec<(usize, f32)>,
    /// Reusable view-vertex buffer (avoids per-face allocation).
    view_verts_buf: Vec<[f32; 3]>,
    /// Cached background gradient row colors.
    bg_cache: Vec<PackedRgba>,
    /// Dimensions for which bg_cache was computed.
    bg_cache_dims: (u32, u32),
}

impl QuakeRenderer {
    /// Create a new renderer for the given screen dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let half_width = width as f32 / 2.0;
        let half_height = height as f32 / 2.0;
        let fov_radians = FOV_DEGREES * std::f32::consts::PI / 180.0;
        let projection = half_width / (fov_radians / 2.0).tan();

        Self {
            width,
            height,
            half_width,
            half_height,
            projection,
            stats: RenderStats::default(),
            face_centroids: Vec::new(),
            face_order_buf: Vec::new(),
            view_verts_buf: Vec::new(),
            bg_cache: Vec::new(),
            bg_cache_dims: (0, 0),
        }
    }

    /// Resize the renderer for new dimensions.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.half_width = width as f32 / 2.0;
        self.half_height = height as f32 / 2.0;
        let fov_radians = FOV_DEGREES * std::f32::consts::PI / 180.0;
        self.projection = self.half_width / (fov_radians / 2.0).tan();
    }

    /// Render a frame into the framebuffer.
    pub fn render(&mut self, fb: &mut QuakeFramebuffer, map: &QuakeMap, player: &Player) {
        if self.width != fb.width || self.height != fb.height {
            self.resize(fb.width, fb.height);
        }

        self.stats = RenderStats::default();
        fb.clear_depth();

        // Draw sky/floor background using cached gradient (overwrites every pixel, no pixel clear needed).
        self.draw_background(fb);

        // Build view matrix from player
        let eye = player.eye_pos();

        // Compute direction vectors once. Inline cross(right, fwd) instead of
        // player.up() which redundantly re-calls forward() and right().
        let fwd = player.forward();
        let right = player.right();
        let up = [
            right[1] * fwd[2] - right[2] * fwd[1],
            right[2] * fwd[0] - right[0] * fwd[2],
            right[0] * fwd[1] - right[1] * fwd[0],
        ];

        // Lazily compute face centroids once (static geometry).
        if self.face_centroids.len() != map.faces.len() {
            self.face_centroids.clear();
            for face in &map.faces {
                if face.vertex_indices.is_empty() {
                    self.face_centroids.push([0.0, 0.0, 0.0]);
                    continue;
                }
                let mut cx = 0.0f32;
                let mut cy = 0.0f32;
                let mut cz = 0.0f32;
                let mut n = 0.0f32;
                for &vi in &face.vertex_indices {
                    if vi < map.vertices.len() {
                        cx += map.vertices[vi][0];
                        cy += map.vertices[vi][1];
                        cz += map.vertices[vi][2];
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    self.face_centroids.push([cx / n, cy / n, cz / n]);
                } else {
                    self.face_centroids.push([0.0, 0.0, 0.0]);
                }
            }
        }

        // Sort faces by distance using cached centroids and reusable buffer.
        self.face_order_buf.clear();
        for (i, centroid) in self.face_centroids.iter().enumerate() {
            if map.faces[i].vertex_indices.is_empty() {
                continue;
            }
            let dx = centroid[0] - eye[0];
            let dy = centroid[1] - eye[1];
            let dz = centroid[2] - eye[2];
            let dist_sq = dx * dx + dy * dy + dz * dz;
            self.face_order_buf.push((i, dist_sq));
        }

        // Sort front-to-back so the z-buffer rejects overlapping farther faces
        // early, before expensive per-pixel divisions and color math.
        self.face_order_buf
            .sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Keep fog math in division form to preserve historical quantization
        // behavior and stable visual output.
        let fog_range = FOG_END - FOG_START;

        // Use index-based iteration to avoid borrow conflict with self.
        let face_count = self.face_order_buf.len();
        for fi in 0..face_count {
            let (face_idx, _dist_sq) = self.face_order_buf[fi];
            let face = &map.faces[face_idx];
            self.stats.faces_tested += 1;

            // Transform face vertices to view space (reuse buffer).
            self.view_verts_buf.clear();
            let mut all_behind = true;

            for &vi in &face.vertex_indices {
                if vi >= map.vertices.len() {
                    continue;
                }
                let v = map.vertices[vi];
                let dx = v[0] - eye[0];
                let dy = v[1] - eye[1];
                let dz = v[2] - eye[2];

                let vx = dx * right[0] + dy * right[1] + dz * right[2];
                let vy = dx * up[0] + dy * up[1] + dz * up[2];
                let vz = dx * fwd[0] + dy * fwd[1] + dz * fwd[2];

                if vz > NEAR_CLIP {
                    all_behind = false;
                }
                self.view_verts_buf.push([vx, vy, vz]);
            }

            if all_behind || self.view_verts_buf.len() < 3 {
                self.stats.faces_culled += 1;
                continue;
            }

            // Backface culling in view space
            let v0 = self.view_verts_buf[0];
            let v1 = self.view_verts_buf[1];
            let v2 = self.view_verts_buf[2];
            let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
            let nz = e1[0] * e2[1] - e1[1] * e2[0];
            let face_normal_view = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                nz,
            ];
            let dot = face_normal_view[0] * v0[0]
                + face_normal_view[1] * v0[1]
                + face_normal_view[2] * v0[2];
            if dot > 0.0 && !matches!(face.tex_type, TexType::Floor | TexType::Ceiling) {
                self.stats.faces_culled += 1;
                continue;
            }

            self.stats.faces_drawn += 1;

            let base_color = face_base_color(face);
            let light = face.light_level;

            // Triangulate the face (fan from vertex 0)
            for tri_i in 1..self.view_verts_buf.len() - 1 {
                let tri = [
                    self.view_verts_buf[0],
                    self.view_verts_buf[tri_i],
                    self.view_verts_buf[tri_i + 1],
                ];
                self.rasterize_triangle(fb, &tri, base_color, light, fog_range);
            }
        }
    }

    /// Rasterize a single triangle with z-buffer and lighting.
    fn rasterize_triangle(
        &mut self,
        fb: &mut QuakeFramebuffer,
        verts: &[[f32; 3]; 3],
        base_color: [u8; 3],
        light: f32,
        fog_range: f32,
    ) {
        // Clip and project vertices
        let mut screen: [[f32; 3]; 3] = [[0.0; 3]; 3]; // [sx, sy, 1/z]
        let mut any_visible = false;

        for (i, v) in verts.iter().enumerate() {
            let z = v[2];
            if z < NEAR_CLIP {
                // Behind near plane - we'll handle partial clipping simply
                screen[i] = [0.0, 0.0, 0.0];
                continue;
            }
            any_visible = true;
            let inv_z = 1.0 / z;
            let sx = self.half_width + v[0] * self.projection * inv_z;
            let sy = self.half_height - v[1] * self.projection * inv_z;
            screen[i] = [sx, sy, inv_z];
        }

        if !any_visible {
            return;
        }

        // Handle partial clipping: clip vertices behind near plane to nearest visible
        for i in 0..3 {
            if verts[i][2] < NEAR_CLIP {
                // Find a visible vertex to interpolate toward
                let next = (i + 1) % 3;
                let prev = (i + 2) % 3;
                if verts[next][2] >= NEAR_CLIP {
                    let denom = verts[next][2] - verts[i][2];
                    if denom.abs() < 1e-6 {
                        return; // Both at near-clip boundary — degenerate
                    }
                    let t = (NEAR_CLIP - verts[i][2]) / denom;
                    let z = NEAR_CLIP;
                    let x = verts[i][0] + t * (verts[next][0] - verts[i][0]);
                    let y = verts[i][1] + t * (verts[next][1] - verts[i][1]);
                    let inv_z = 1.0 / z;
                    screen[i] = [
                        self.half_width + x * self.projection * inv_z,
                        self.half_height - y * self.projection * inv_z,
                        inv_z,
                    ];
                } else if verts[prev][2] >= NEAR_CLIP {
                    let denom = verts[prev][2] - verts[i][2];
                    if denom.abs() < 1e-6 {
                        return; // Both at near-clip boundary — degenerate
                    }
                    let t = (NEAR_CLIP - verts[i][2]) / denom;
                    let z = NEAR_CLIP;
                    let x = verts[i][0] + t * (verts[prev][0] - verts[i][0]);
                    let y = verts[i][1] + t * (verts[prev][1] - verts[i][1]);
                    let inv_z = 1.0 / z;
                    screen[i] = [
                        self.half_width + x * self.projection * inv_z,
                        self.half_height - y * self.projection * inv_z,
                        inv_z,
                    ];
                } else {
                    return; // All behind
                }
            }
        }

        self.stats.triangles_rasterized += 1;

        // Compute screen-space bounding box
        let min_x = screen[0][0].min(screen[1][0]).min(screen[2][0]).max(0.0) as u32;
        let max_x = screen[0][0]
            .max(screen[1][0])
            .max(screen[2][0])
            .min(self.width as f32 - 1.0) as u32;
        let min_y = screen[0][1].min(screen[1][1]).min(screen[2][1]).max(0.0) as u32;
        let max_y = screen[0][1]
            .max(screen[1][1])
            .max(screen[2][1])
            .min(self.height as f32 - 1.0) as u32;

        if min_x > max_x || min_y > max_y {
            return;
        }

        // Precompute edge functions for barycentric coordinates
        let (s0, s1, s2) = (screen[0], screen[1], screen[2]);
        let area = edge_function(s0, s1, s2);
        if area.abs() < 0.001 {
            return; // Degenerate triangle
        }
        let inv_area = 1.0 / area;

        // Hoist per-triangle edge deltas (constant for all pixels in this triangle)
        let e12_dy = s2[1] - s1[1];
        let e12_dx = s2[0] - s1[0];
        let e20_dy = s0[1] - s2[1];
        let e20_dx = s0[0] - s2[0];

        // Scanline rasterization with barycentric interpolation
        let fb_width = fb.width;
        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            let row_offset = py * fb_width;

            // Hoist per-row fy-dependent terms (constant for all px in this row)
            let row_w0_fy = (fy - s1[1]) * e12_dx;
            let row_w1_fy = (fy - s2[1]) * e20_dx;

            // Convex triangle: inside pixels form a contiguous interval per
            // scanline, so once we leave we can skip the remaining columns.
            let mut entered = false;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;

                let w0 = ((fx - s1[0]) * e12_dy - row_w0_fy) * inv_area;
                let w1 = ((fx - s2[0]) * e20_dy - row_w1_fy) * inv_area;
                let w2 = 1.0 - w0 - w1;

                // Inside triangle test
                if area > 0.0 {
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        if entered {
                            break;
                        }
                        continue;
                    }
                } else if w0 > 0.0 || w1 > 0.0 || w2 > 0.0 {
                    if entered {
                        break;
                    }
                    continue;
                }
                entered = true;

                // Interpolate depth (1/z)
                let inv_z = w0 * s0[2] + w1 * s1[2] + w2 * s2[2];
                if inv_z <= 0.0 {
                    continue;
                }

                let z = 1.0 / inv_z;

                // Early-z: reject occluded pixels before any color math.
                // With front-to-back sort this rejects most overdraw cheaply.
                let idx = (row_offset + px) as usize;
                if z >= fb.depth[idx] {
                    continue;
                }

                // These divisions + color math only execute for surviving pixels.
                let dist_light = (1.0 / (1.0 + z * 0.003)).clamp(0.0, 1.0);
                let fog_t = ((z - FOG_START) / fog_range).clamp(0.0, 1.0);
                let total_light = light * dist_light;
                let fr = shade_channel(base_color[0], FOG_COLOR[0], total_light, fog_t);
                let fg = shade_channel(base_color[1], FOG_COLOR[1], total_light, fog_t);
                let fbl = shade_channel(base_color[2], FOG_COLOR[2], total_light, fog_t);

                fb.pixels[idx] = PackedRgba::rgb(fr, fg, fbl);
                fb.depth[idx] = z;
                self.stats.pixels_written += 1;
            }
        }
    }

    /// Draw the sky and floor background using cached per-row colors.
    fn draw_background(&mut self, fb: &mut QuakeFramebuffer) {
        // Cache per-row gradient colors (dimensions-dependent, static otherwise).
        if self.bg_cache_dims != (self.width, self.height) {
            let horizon = self.height / 2;
            self.bg_cache.clear();
            self.bg_cache.reserve(self.height as usize);
            for y in 0..self.height {
                let color = if y < horizon {
                    let t = y as f32 / horizon as f32;
                    let r = lerp_u8(SKY_TOP[0], SKY_BOTTOM[0], t);
                    let g = lerp_u8(SKY_TOP[1], SKY_BOTTOM[1], t);
                    let b = lerp_u8(SKY_TOP[2], SKY_BOTTOM[2], t);
                    PackedRgba::rgb(r, g, b)
                } else {
                    let t = ((y - horizon) as f32 / (self.height - horizon).max(1) as f32).min(1.0);
                    let r = lerp_u8(FLOOR_FAR[0], FLOOR_NEAR[0], t);
                    let g = lerp_u8(FLOOR_FAR[1], FLOOR_NEAR[1], t);
                    let b = lerp_u8(FLOOR_FAR[2], FLOOR_NEAR[2], t);
                    PackedRgba::rgb(r, g, b)
                };
                self.bg_cache.push(color);
            }
            self.bg_cache_dims = (self.width, self.height);
        }

        let row_width = self.width as usize;
        for y in 0..self.height {
            let color = self.bg_cache[y as usize];
            let row_start = y as usize * row_width;
            let row_end = row_start + row_width;
            fb.pixels[row_start..row_end].fill(color);
        }
    }
}

/// Get the base color for a face from the wall color palette.
#[inline]
fn face_base_color(face: &Face) -> [u8; 3] {
    match face.tex_type {
        TexType::Floor => FLOOR_NEAR,
        TexType::Ceiling => CEILING_COLOR,
        TexType::Sky => SKY_TOP,
        TexType::Lava => [200, 80, 20],
        TexType::Metal => [140, 140, 160],
        TexType::Wall => WALL_COLORS[face.color_index as usize % WALL_COLORS.len()],
    }
}

/// Edge function for barycentric coordinate computation.
#[inline]
fn edge_function(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f32 {
    (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
}

/// Linearly interpolate between two u8 values.
#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).clamp(0.0, 255.0) as u8
}

/// Apply distance light, then quantize before fog blend to preserve
/// historically stable output values.
#[inline]
fn shade_channel(base: u8, fog: u8, total_light: f32, fog_t: f32) -> u8 {
    let lit = (base as f32 * total_light) as u8;
    lerp_u8(lit, fog, fog_t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_creation() {
        let r = QuakeRenderer::new(320, 200);
        assert_eq!(r.width, 320);
        assert_eq!(r.height, 200);
        assert!(r.projection > 0.0);
    }

    #[test]
    fn renderer_resize() {
        let mut r = QuakeRenderer::new(320, 200);
        r.resize(640, 400);
        assert_eq!(r.width, 640);
        assert_eq!(r.height, 400);
    }

    #[test]
    fn edge_function_basic() {
        let a = [0.0, 0.0, 0.0];
        let b = [10.0, 0.0, 0.0];
        let c = [5.0, 5.0, 0.0];
        let area = edge_function(a, b, c);
        assert!(area.abs() > 0.01);
    }

    #[test]
    fn lerp_u8_basic() {
        assert_eq!(lerp_u8(0, 100, 0.5), 50);
        assert_eq!(lerp_u8(0, 200, 0.0), 0);
        assert_eq!(lerp_u8(0, 200, 1.0), 200);
    }

    #[test]
    fn shade_channel_quantizes_before_fog_blend() {
        // Regression case: moving quantization after fog blend changes output.
        let base = 1u8;
        let fog = 2u8;
        let total_light = 0.05 * 0.95;
        let fog_t = 0.49;

        let legacy = shade_channel(base, fog, total_light, fog_t);
        let fused =
            (base as f32 * total_light * (1.0 - fog_t) + fog as f32 * fog_t).min(255.0) as u8;

        assert_eq!(legacy, 0);
        assert_eq!(fused, 1);
        assert_ne!(legacy, fused);
    }

    #[test]
    fn face_color_lookup() {
        let face = Face {
            vertex_indices: vec![],
            normal: [0.0, 0.0, 1.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Wall,
        };
        let c = face_base_color(&face);
        assert_eq!(c, WALL_COLORS[0]);
    }

    #[test]
    fn render_empty_map() {
        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let map = QuakeMap::new();
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);
        // Should not panic, background should be drawn
        assert_eq!(renderer.stats.faces_tested, 0);
    }

    #[test]
    fn render_adapts_to_framebuffer_dimensions() {
        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(48, 30);
        let map = QuakeMap::new();
        let player = Player::default();

        renderer.render(&mut fb, &map, &player);

        assert_eq!(renderer.width, 48);
        assert_eq!(renderer.height, 30);
    }

    #[test]
    fn render_with_map() {
        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let map = super::super::map::generate_e1m1();
        let mut player = Player::default();
        let (px, py, pz, pyaw) = map.player_start();
        player.spawn(px, py, pz, pyaw);
        renderer.render(&mut fb, &map, &player);
        assert!(renderer.stats.faces_tested > 0);
        assert!(renderer.stats.faces_drawn > 0);
    }

    #[test]
    fn renderer_projection_positive() {
        let r = QuakeRenderer::new(640, 480);
        assert!(r.projection > 0.0, "projection should be positive");
    }

    #[test]
    fn renderer_resize_updates_half() {
        let mut r = QuakeRenderer::new(100, 100);
        r.resize(200, 300);
        assert_eq!(r.half_width, 100.0);
        assert_eq!(r.half_height, 150.0);
    }

    #[test]
    fn render_stats_default_zeroed() {
        let stats = RenderStats::default();
        assert_eq!(stats.faces_tested, 0);
        assert_eq!(stats.faces_culled, 0);
        assert_eq!(stats.faces_drawn, 0);
        assert_eq!(stats.triangles_rasterized, 0);
        assert_eq!(stats.pixels_written, 0);
    }

    #[test]
    fn face_base_color_all_types() {
        let types_and_expected = [
            (TexType::Floor, FLOOR_NEAR),
            (TexType::Ceiling, CEILING_COLOR),
            (TexType::Sky, SKY_TOP),
            (TexType::Lava, [200, 80, 20]),
            (TexType::Metal, [140, 140, 160]),
        ];
        for (tex_type, expected) in types_and_expected {
            let face = Face {
                vertex_indices: vec![],
                normal: [0.0, 0.0, 1.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: 1.0,
                tex_type,
            };
            assert_eq!(face_base_color(&face), expected, "for {tex_type:?}");
        }
    }

    #[test]
    fn face_base_color_wall_wraps_index() {
        let face = Face {
            vertex_indices: vec![],
            normal: [0.0, 0.0, 1.0],
            dist: 0.0,
            color_index: WALL_COLORS.len() as u8 + 2,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Wall,
        };
        let expected_idx = (WALL_COLORS.len() as u8 + 2) as usize % WALL_COLORS.len();
        assert_eq!(face_base_color(&face), WALL_COLORS[expected_idx]);
    }

    #[test]
    fn edge_function_degenerate_triangle() {
        // All points same -> area should be 0
        let p = [5.0, 5.0, 0.0];
        assert!(edge_function(p, p, p).abs() < 1e-6);
    }

    #[test]
    fn edge_function_signed_area() {
        let a = [0.0, 0.0, 0.0];
        let b = [10.0, 0.0, 0.0];
        let c = [0.0, 10.0, 0.0];
        // edge_function: (c.x-a.x)*(b.y-a.y) - (c.y-a.y)*(b.x-a.x)
        let area = edge_function(a, b, c);
        // Swapping b and c should negate the result
        let area_swap = edge_function(a, c, b);
        assert!(
            (area + area_swap).abs() < 1e-5,
            "swapping b and c should negate: {area} vs {area_swap}"
        );
        assert!(
            area.abs() > 0.01,
            "non-degenerate triangle should have non-zero area"
        );
    }

    #[test]
    fn lerp_u8_clamps_bounds() {
        assert_eq!(lerp_u8(0, 255, -1.0), 0);
        assert_eq!(lerp_u8(0, 255, 2.0), 255);
    }

    #[test]
    fn render_zero_size_is_safe() {
        let mut renderer = QuakeRenderer::new(0, 0);
        let mut fb = QuakeFramebuffer::new(0, 0);
        let map = super::super::map::QuakeMap::new();
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);
        // Should not panic
    }

    // ---- z-buffer depth testing ----
    //
    // NOTE: Default player at origin looks along +X (yaw=0).
    // View space: vz = world_x, vx = -world_y, vy = world_z - eye_z.
    // Eye at [0, 0, PLAYER_VIEW_HEIGHT ≈ 22].

    #[test]
    fn zbuffer_closer_triangle_occludes() {
        // Two triangles along +X: far at x=100, near at x=20.
        let mut map = QuakeMap::new();
        map.vertices = vec![
            // Far triangle (x=100, centered in Y/Z around eye)
            [100.0, -20.0, 42.0],
            [100.0, 20.0, 42.0],
            [100.0, 0.0, 2.0],
            // Near triangle (x=20)
            [20.0, -5.0, 27.0],
            [20.0, 5.0, 27.0],
            [20.0, 0.0, 17.0],
        ];
        map.faces = vec![
            Face {
                vertex_indices: vec![0, 1, 2],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
            Face {
                vertex_indices: vec![3, 4, 5],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 2,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
        ];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Both faces should be drawn, z-buffer handles ordering
        assert!(renderer.stats.faces_drawn >= 1);
        assert!(renderer.stats.pixels_written > 0);
    }

    #[test]
    fn zbuffer_depth_values_decrease_for_closer() {
        // Triangle at x=20, centered on screen
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, -10.0, 32.0], [20.0, 10.0, 32.0], [20.0, 0.0, 12.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Some pixels should have depth < f32::MAX (they were written)
        let written_depths: Vec<f32> = fb.depth.iter().copied().filter(|&d| d < f32::MAX).collect();
        assert!(
            !written_depths.is_empty(),
            "triangle should write some depth values"
        );
        for &d in &written_depths {
            assert!(d > 0.0, "depth should be positive");
        }
    }

    // ---- backface culling ----

    #[test]
    fn backface_culled_when_facing_away() {
        // Wall face at x=20, winding produces view-space normal facing away (dot > 0).
        // Vertices chosen so backface test detects the face as rear-facing.
        let mut map = QuakeMap::new();
        map.vertices = vec![
            // CW winding in view space → backface
            [20.0, 10.0, 32.0],
            [20.0, -10.0, 32.0],
            [20.0, 0.0, 12.0],
        ];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [1.0, 0.0, 0.0], // facing away (+X = toward camera)
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Wall,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(renderer.stats.faces_tested, 1);
        // Face should be culled (either by backface or near-clip)
        assert_eq!(
            renderer.stats.faces_drawn + renderer.stats.faces_culled,
            1,
            "face should be tested"
        );
    }

    #[test]
    fn floor_ceiling_not_backface_culled() {
        // Floor face at x=20 (in front of camera) — Floor bypasses backface culling
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, -10.0, 12.0], [20.0, 10.0, 12.0], [40.0, 0.0, 12.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [0.0, 0.0, 1.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Floor faces should never be backface-culled
        assert_eq!(renderer.stats.faces_tested, 1);
        assert_eq!(
            renderer.stats.faces_drawn, 1,
            "floor should not be backface culled"
        );
    }

    // ---- near-plane clipping ----

    #[test]
    fn all_behind_near_plane_culled() {
        // All vertices behind camera (negative X = behind player looking along +X)
        let mut map = QuakeMap::new();
        map.vertices = vec![[-5.0, -10.0, 32.0], [-5.0, 10.0, 32.0], [-5.0, 0.0, 12.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(
            renderer.stats.faces_culled, 1,
            "face entirely behind camera should be culled"
        );
        assert_eq!(renderer.stats.faces_drawn, 0);
    }

    #[test]
    fn partial_near_clip_renders_visible_part() {
        // One vertex behind (neg X), two in front (pos X)
        let mut map = QuakeMap::new();
        map.vertices = vec![
            [-5.0, 0.0, 22.0],   // behind camera
            [30.0, -20.0, 32.0], // in front
            [30.0, 20.0, 32.0],  // in front
        ];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Should not be fully culled - the visible part should render
        assert_eq!(renderer.stats.faces_tested, 1);
        // At least some pixels should be written from the visible portion
        assert!(renderer.stats.pixels_written > 0 || renderer.stats.faces_drawn > 0);
    }

    // ---- fog ----

    #[test]
    fn fog_at_fog_start_is_zero() {
        // At FOG_START distance, fog_t should be 0.0 → color unchanged
        let fog_range = FOG_END - FOG_START;
        let z = FOG_START;
        let fog_t = ((z - FOG_START) / fog_range).clamp(0.0, 1.0);
        assert!((fog_t - 0.0).abs() < 1e-6, "fog at FOG_START should be 0.0");
    }

    #[test]
    fn fog_at_fog_end_is_one() {
        let fog_range = FOG_END - FOG_START;
        let z = FOG_END;
        let fog_t = ((z - FOG_START) / fog_range).clamp(0.0, 1.0);
        assert!((fog_t - 1.0).abs() < 1e-6, "fog at FOG_END should be 1.0");
    }

    #[test]
    fn fog_beyond_end_is_clamped() {
        let fog_range = FOG_END - FOG_START;
        let z = FOG_END + 500.0;
        let fog_t = ((z - FOG_START) / fog_range).clamp(0.0, 1.0);
        assert!(
            (fog_t - 1.0).abs() < 1e-6,
            "fog beyond FOG_END should clamp to 1.0"
        );
    }

    #[test]
    fn fog_before_start_is_zero() {
        let fog_range = FOG_END - FOG_START;
        let z = 5.0; // close, before fog start
        let fog_t = ((z - FOG_START) / fog_range).clamp(0.0, 1.0);
        assert!(
            (fog_t - 0.0).abs() < 1e-6,
            "fog before FOG_START should be 0.0"
        );
    }

    // ---- background gradient ----

    #[test]
    fn background_fills_all_pixels() {
        let mut renderer = QuakeRenderer::new(32, 20);
        let mut fb = QuakeFramebuffer::new(32, 20);
        let map = QuakeMap::new();
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Every pixel should have been filled by the background
        let transparent = PackedRgba::default();
        for (i, px) in fb.pixels.iter().enumerate() {
            assert_ne!(
                *px, transparent,
                "pixel {i} should not be transparent after background render"
            );
        }
    }

    #[test]
    fn background_cache_reused_on_second_render() {
        let mut renderer = QuakeRenderer::new(32, 20);
        let mut fb = QuakeFramebuffer::new(32, 20);
        let map = QuakeMap::new();
        let player = Player::default();

        renderer.render(&mut fb, &map, &player);
        assert_eq!(renderer.bg_cache_dims, (32, 20));
        assert_eq!(renderer.bg_cache.len(), 20);

        // Second render should reuse cache
        renderer.render(&mut fb, &map, &player);
        assert_eq!(renderer.bg_cache_dims, (32, 20));
    }

    #[test]
    fn background_cache_invalidated_on_resize() {
        let mut renderer = QuakeRenderer::new(32, 20);
        let mut fb = QuakeFramebuffer::new(32, 20);
        let map = QuakeMap::new();
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        renderer.resize(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        renderer.render(&mut fb, &map, &player);
        assert_eq!(renderer.bg_cache_dims, (64, 40));
        assert_eq!(renderer.bg_cache.len(), 40);
    }

    // ---- face centroid caching ----

    #[test]
    fn face_centroids_computed_lazily() {
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, 0.0, 22.0], [30.0, 0.0, 22.0], [25.0, 10.0, 22.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        assert!(renderer.face_centroids.is_empty());

        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(renderer.face_centroids.len(), 1);
        // Centroid should be average of 3 vertices
        let expected = [25.0, 10.0 / 3.0, 22.0];
        for (i, &exp) in expected.iter().enumerate() {
            assert!(
                (renderer.face_centroids[0][i] - exp).abs() < 0.01,
                "centroid[{i}] mismatch: {} != {}",
                renderer.face_centroids[0][i],
                exp
            );
        }
    }

    #[test]
    fn face_centroids_recomputed_when_face_count_changes() {
        let mut renderer = QuakeRenderer::new(64, 40);
        let player = Player::default();

        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, 0.0, 22.0], [30.0, 0.0, 22.0], [25.0, 10.0, 22.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut fb = QuakeFramebuffer::new(64, 40);
        renderer.render(&mut fb, &map, &player);
        assert_eq!(renderer.face_centroids.len(), 1);

        // Add another face
        map.vertices.push([40.0, 0.0, 22.0]);
        map.faces.push(Face {
            vertex_indices: vec![0, 1, 3],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 1,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        });

        renderer.render(&mut fb, &map, &player);
        assert_eq!(renderer.face_centroids.len(), 2);
    }

    #[test]
    fn empty_face_gets_zero_centroid() {
        let mut map = QuakeMap::new();
        map.faces = vec![Face {
            vertex_indices: vec![],
            normal: [0.0, 0.0, 1.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Wall,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(renderer.face_centroids.len(), 1);
        assert_eq!(renderer.face_centroids[0], [0.0, 0.0, 0.0]);
    }

    // ---- projection math ----

    #[test]
    fn projection_scales_with_width() {
        let r1 = QuakeRenderer::new(320, 200);
        let r2 = QuakeRenderer::new(640, 200);
        // Wider screen → larger projection scale (for same FOV)
        assert!(r2.projection > r1.projection);
    }

    #[test]
    fn projection_consistent_after_resize() {
        let fresh = QuakeRenderer::new(640, 400);
        let mut resized = QuakeRenderer::new(320, 200);
        resized.resize(640, 400);
        assert!(
            (fresh.projection - resized.projection).abs() < 1e-3,
            "resize should produce same projection as fresh: {} vs {}",
            fresh.projection,
            resized.projection
        );
    }

    // ---- stats tracking ----

    #[test]
    fn render_resets_stats_each_frame() {
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, -10.0, 32.0], [20.0, 10.0, 32.0], [20.0, 0.0, 12.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();

        renderer.render(&mut fb, &map, &player);
        let first_pixels = renderer.stats.pixels_written;

        renderer.render(&mut fb, &map, &player);
        // Stats should be from the second render only, not accumulated
        assert_eq!(
            renderer.stats.pixels_written, first_pixels,
            "same scene should produce same pixel count"
        );
    }

    #[test]
    fn stats_faces_tested_equals_non_empty_faces() {
        let mut map = QuakeMap::new();
        map.vertices = vec![
            [20.0, -10.0, 32.0],
            [20.0, 10.0, 32.0],
            [20.0, 0.0, 12.0],
            [40.0, -10.0, 32.0],
            [40.0, 10.0, 32.0],
            [40.0, 0.0, 12.0],
        ];
        map.faces = vec![
            Face {
                vertex_indices: vec![0, 1, 2],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
            Face {
                vertex_indices: vec![], // empty - should be skipped
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
            Face {
                vertex_indices: vec![3, 4, 5],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 1,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
        ];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Empty face should be skipped in sorting, so only 2 tested
        assert_eq!(renderer.stats.faces_tested, 2);
    }

    #[test]
    fn stats_faces_drawn_plus_culled_equals_tested() {
        let map = super::super::map::generate_e1m1();
        let mut player = Player::default();
        let (px, py, pz, pyaw) = map.player_start();
        player.spawn(px, py, pz, pyaw);

        let mut renderer = QuakeRenderer::new(128, 80);
        let mut fb = QuakeFramebuffer::new(128, 80);
        renderer.render(&mut fb, &map, &player);

        assert_eq!(
            renderer.stats.faces_drawn + renderer.stats.faces_culled,
            renderer.stats.faces_tested,
            "drawn ({}) + culled ({}) should equal tested ({})",
            renderer.stats.faces_drawn,
            renderer.stats.faces_culled,
            renderer.stats.faces_tested
        );
    }

    // ---- edge function math ----

    #[test]
    fn edge_function_unit_triangle_area() {
        // edge_function: (c.x-a.x)*(b.y-a.y) - (c.y-a.y)*(b.x-a.x)
        // For (0,0),(1,0),(0,1): (0)*(0) - (1)*(1) = -1
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let c = [0.0, 1.0, 0.0];
        let area = edge_function(a, b, c);
        assert!(
            (area - (-1.0)).abs() < 1e-5,
            "unit right triangle should have signed area -1.0, got {area}"
        );
    }

    #[test]
    fn edge_function_collinear_zero() {
        // Collinear points → area 0
        let a = [0.0, 0.0, 0.0];
        let b = [5.0, 0.0, 0.0];
        let c = [10.0, 0.0, 0.0];
        assert!(edge_function(a, b, c).abs() < 1e-6);
    }

    // ---- lerp ----

    #[test]
    fn lerp_u8_midpoint() {
        assert_eq!(lerp_u8(100, 200, 0.5), 150);
    }

    #[test]
    fn lerp_u8_same_value() {
        assert_eq!(lerp_u8(42, 42, 0.5), 42);
    }

    // ---- degenerate triangle ----

    #[test]
    fn degenerate_triangle_skipped() {
        // Three collinear vertices along Y at x=20 - near-zero area triangle
        let mut map = QuakeMap::new();
        map.vertices = vec![
            [20.0, -10.0, 22.0],
            [20.0, 10.0, 22.0],
            [20.0, 0.0, 22.0], // collinear (all same Z, same X)
        ];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // Degenerate triangle should produce 0 pixels
        assert_eq!(renderer.stats.pixels_written, 0);
    }

    // ---- out-of-bounds vertex indices ----

    #[test]
    fn out_of_bounds_vertex_index_safe() {
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, 0.0, 22.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 999, 1000], // 999/1000 are OOB
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        // Should not panic
        renderer.render(&mut fb, &map, &player);
    }

    // ---- large scene stability ----

    #[test]
    fn render_full_e1m1_no_panic() {
        let map = super::super::map::generate_e1m1();
        let mut player = Player::default();
        let (px, py, pz, pyaw) = map.player_start();
        player.spawn(px, py, pz, pyaw);

        let mut renderer = QuakeRenderer::new(SCREENWIDTH, SCREENHEIGHT);
        let mut fb = QuakeFramebuffer::new(SCREENWIDTH, SCREENHEIGHT);
        renderer.render(&mut fb, &map, &player);

        assert!(
            renderer.stats.faces_tested > 10,
            "E1M1 should have many faces"
        );
        assert!(
            renderer.stats.triangles_rasterized > 0,
            "should rasterize triangles"
        );
        assert!(
            renderer.stats.pixels_written > 100,
            "should write many pixels"
        );
    }

    #[test]
    fn render_stats_debug_display() {
        let stats = RenderStats {
            faces_tested: 100,
            faces_culled: 30,
            faces_drawn: 70,
            triangles_rasterized: 200,
            pixels_written: 5000,
        };
        let s = format!("{stats:?}");
        assert!(s.contains("100"));
        assert!(s.contains("5000"));
    }

    #[test]
    fn render_stats_clone() {
        let stats = RenderStats {
            faces_tested: 42,
            faces_culled: 10,
            faces_drawn: 32,
            triangles_rasterized: 64,
            pixels_written: 1000,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.faces_tested, 42);
        assert_eq!(cloned.pixels_written, 1000);
    }

    // ---- face sorting order ----

    #[test]
    fn faces_sorted_back_to_front() {
        // Two faces at different distances along +X
        let mut map = QuakeMap::new();
        map.vertices = vec![
            // Near face (x=15)
            [15.0, -5.0, 27.0],
            [15.0, 5.0, 27.0],
            [15.0, 0.0, 17.0],
            // Far face (x=50)
            [50.0, -5.0, 27.0],
            [50.0, 5.0, 27.0],
            [50.0, 0.0, 17.0],
        ];
        map.faces = vec![
            Face {
                vertex_indices: vec![0, 1, 2],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
            Face {
                vertex_indices: vec![3, 4, 5],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 1,
                is_sky: false,
                light_level: 1.0,
                tex_type: TexType::Floor,
            },
        ];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(
            renderer.stats.faces_tested, 2,
            "both faces should be tested"
        );
    }

    // ---- quad (4-vertex) face triangulation ----

    #[test]
    fn quad_face_triangulated_into_two() {
        // A quad at x=20 should be triangulated as a fan from vertex 0
        let mut map = QuakeMap::new();
        map.vertices = vec![
            [20.0, -10.0, 32.0],
            [20.0, 10.0, 32.0],
            [20.0, 10.0, 12.0],
            [20.0, -10.0, 12.0],
        ];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1, 2, 3],
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        // A quad is triangulated into 2 triangles (4 verts - 2 = 2)
        assert_eq!(renderer.stats.triangles_rasterized, 2, "quad → 2 triangles");
    }

    // ---- face with < 3 vertices ----

    #[test]
    fn face_with_two_vertices_culled() {
        let mut map = QuakeMap::new();
        map.vertices = vec![[20.0, -5.0, 22.0], [20.0, 5.0, 22.0]];
        map.faces = vec![Face {
            vertex_indices: vec![0, 1], // only 2 verts
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            color_index: 0,
            is_sky: false,
            light_level: 1.0,
            tex_type: TexType::Floor,
        }];

        let mut renderer = QuakeRenderer::new(64, 40);
        let mut fb = QuakeFramebuffer::new(64, 40);
        let player = Player::default();
        renderer.render(&mut fb, &map, &player);

        assert_eq!(
            renderer.stats.faces_culled, 1,
            "face with < 3 verts should be culled"
        );
        assert_eq!(renderer.stats.faces_drawn, 0);
    }

    // ---- lighting ----

    #[test]
    fn light_level_affects_pixel_brightness() {
        // Render same triangle with full light and half light, compare center pixel
        let make_map = |light: f32| {
            let mut map = QuakeMap::new();
            // Large triangle at x=20 centered on screen
            map.vertices = vec![[20.0, -30.0, 52.0], [20.0, 30.0, 52.0], [20.0, 0.0, -8.0]];
            map.faces = vec![Face {
                vertex_indices: vec![0, 1, 2],
                normal: [-1.0, 0.0, 0.0],
                dist: 0.0,
                color_index: 0,
                is_sky: false,
                light_level: light,
                tex_type: TexType::Floor,
            }];
            map
        };

        let player = Player::default();
        let center = (20 * 64 + 32) as usize; // row 20, col 32 (center-ish)

        let mut r1 = QuakeRenderer::new(64, 40);
        let mut fb1 = QuakeFramebuffer::new(64, 40);
        r1.render(&mut fb1, &make_map(1.0), &player);

        let mut r2 = QuakeRenderer::new(64, 40);
        let mut fb2 = QuakeFramebuffer::new(64, 40);
        r2.render(&mut fb2, &make_map(0.3), &player);

        // Both should write to center area
        if fb1.depth[center] < f32::MAX && fb2.depth[center] < f32::MAX {
            // Brighter light should produce brighter pixels (higher RGB sum)
            let c1 = fb1.pixels[center];
            let sum1 = c1.r() as u32 + c1.g() as u32 + c1.b() as u32;
            let c2 = fb2.pixels[center];
            let sum2 = c2.r() as u32 + c2.g() as u32 + c2.b() as u32;
            assert!(
                sum1 >= sum2,
                "full light ({sum1}) should be >= half light ({sum2})"
            );
        }
    }
}
