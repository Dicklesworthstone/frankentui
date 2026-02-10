use std::f32::consts::TAU;

use ftui_extras::canvas::Painter;
use ftui_extras::visual_fx::FxQuality;
use ftui_render::cell::PackedRgba;

mod three_d_data {
    include!("3d_data.rs");
}
use three_d_data::{QUAKE_E1M1_TRIS, QUAKE_E1M1_VERTS};

#[derive(Debug, Clone, Copy)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    #[must_use]
    pub fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    pub fn len(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    #[must_use]
    pub fn normalized(self) -> Self {
        let len = self.len();
        if len > 0.0 {
            Self::new(self.x / len, self.y / len, self.z / len)
        } else {
            self
        }
    }
}

impl core::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl core::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl core::ops::Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

const QUAKE_EYE_HEIGHT: f32 = 0.18;
const QUAKE_GRAVITY: f32 = -0.28;
const QUAKE_JUMP_VELOCITY: f32 = 0.22;
const QUAKE_COLLISION_RADIUS: f32 = 0.06;
const QUAKE_MOVE_SPEED: f32 = 0.08;
const QUAKE_STRAFE_SPEED: f32 = 0.07;
const QUAKE_FRICTION: f32 = 0.85;
const QUAKE_ACCEL: f32 = 0.02;

fn cross2(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ax * by - ay * bx
}

fn point_segment_distance_sq(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let vx = x2 - x1;
    let vy = y2 - y1;
    let wx = px - x1;
    let wy = py - y1;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= 1e-6 {
        let dx = px - x1;
        let dy = py - y1;
        return dx * dx + dy * dy;
    }
    let t = ((wx * vx) + (wy * vy)) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let proj_x = x1 + t * vx;
    let proj_y = y1 + t * vy;
    let dx = px - proj_x;
    let dy = py - proj_y;
    dx * dx + dy * dy
}

fn clip_polygon_near(poly: &[Vec3], near: f32) -> Vec<Vec3> {
    if poly.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(poly.len() + 2);
    let mut prev = poly[poly.len() - 1];
    let mut prev_inside = prev.z >= near;

    for &curr in poly {
        let curr_inside = curr.z >= near;
        if prev_inside && curr_inside {
            out.push(curr);
        } else if prev_inside && !curr_inside {
            let denom = curr.z - prev.z;
            if denom.abs() > 1e-6 {
                let t = (near - prev.z) / denom;
                out.push(Vec3::new(
                    prev.x + (curr.x - prev.x) * t,
                    prev.y + (curr.y - prev.y) * t,
                    near,
                ));
            }
        } else if !prev_inside && curr_inside {
            let denom = curr.z - prev.z;
            if denom.abs() > 1e-6 {
                let t = (near - prev.z) / denom;
                out.push(Vec3::new(
                    prev.x + (curr.x - prev.x) * t,
                    prev.y + (curr.y - prev.y) * t,
                    near,
                ));
            }
            out.push(curr);
        }
        prev = curr;
        prev_inside = curr_inside;
    }

    out
}

fn palette_quake_stone(t: f64) -> PackedRgba {
    // Quake palette approximation (browns, tans, greys)
    let t = t.clamp(0.0, 1.0);

    // Base colors from Quake palette
    let c1 = (47, 43, 35); // Dark mud
    let c2 = (83, 75, 60); // Mid brown
    let c3 = (131, 120, 95); // Tan
    let c4 = (110, 100, 90); // Grey-ish stone

    let (r, g, b) = if t < 0.33 {
        let ft = t / 0.33;
        (
            c1.0 as f64 * (1.0 - ft) + c2.0 as f64 * ft,
            c1.1 as f64 * (1.0 - ft) + c2.1 as f64 * ft,
            c1.2 as f64 * (1.0 - ft) + c2.2 as f64 * ft,
        )
    } else if t < 0.66 {
        let ft = (t - 0.33) / 0.33;
        (
            c2.0 as f64 * (1.0 - ft) + c3.0 as f64 * ft,
            c2.1 as f64 * (1.0 - ft) + c3.1 as f64 * ft,
            c2.2 as f64 * (1.0 - ft) + c3.2 as f64 * ft,
        )
    } else {
        let ft = (t - 0.66) / 0.34;
        (
            c3.0 as f64 * (1.0 - ft) + c4.0 as f64 * ft,
            c3.1 as f64 * (1.0 - ft) + c4.1 as f64 * ft,
            c3.2 as f64 * (1.0 - ft) + c4.2 as f64 * ft,
        )
    };

    PackedRgba::rgb(r as u8, g as u8, b as u8)
}

#[derive(Debug, Clone, Copy)]
struct WallSeg {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

#[derive(Debug, Clone)]
struct FloorTri {
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    area: f32,
}

impl FloorTri {
    fn new(v0: Vec3, v1: Vec3, v2: Vec3) -> Option<Self> {
        let area = cross2(v1.x - v0.x, v1.y - v0.y, v2.x - v0.x, v2.y - v0.y);
        if area.abs() <= 1e-6 {
            return None;
        }
        let min_x = v0.x.min(v1.x).min(v2.x);
        let max_x = v0.x.max(v1.x).max(v2.x);
        let min_y = v0.y.min(v1.y).min(v2.y);
        let max_y = v0.y.max(v1.y).max(v2.y);
        Some(Self {
            v0,
            v1,
            v2,
            min_x,
            max_x,
            min_y,
            max_y,
            area,
        })
    }
}

#[derive(Debug, Clone)]
pub struct QuakePlayer {
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
}

impl QuakePlayer {
    fn new(pos: Vec3) -> Self {
        Self {
            pos,
            vel: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            grounded: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuakeE1M1State {
    pub player: QuakePlayer,
    fire_flash: f32,
    bounds_min: Vec3,
    bounds_max: Vec3,
    wall_segments: Vec<WallSeg>,
    floor_tris: Vec<FloorTri>,
    depth: Vec<f32>,
    depth_w: u16,
    depth_h: u16,
    // Input state
    move_fwd: f32,
    move_side: f32,
}

impl Default for QuakeE1M1State {
    fn default() -> Self {
        let (min, max) = Self::compute_bounds();
        let (wall_segments, floor_tris) = Self::build_collision();
        let center_x = (min.x + max.x) * 0.5;
        let center_y = (min.y + max.y) * 0.5;
        let start = Vec3::new(center_x, center_y, min.z + QUAKE_EYE_HEIGHT);
        let mut state = Self {
            player: QuakePlayer::new(start),
            fire_flash: 0.0,
            bounds_min: min,
            bounds_max: max,
            wall_segments,
            floor_tris,
            depth: Vec::new(),
            depth_w: 0,
            depth_h: 0,
            move_fwd: 0.0,
            move_side: 0.0,
        };
        let ground = state.ground_eye_height(center_x, center_y);
        state.player.pos = Vec3::new(center_x, center_y, ground);
        state
    }
}

impl QuakeE1M1State {
    fn compute_bounds() -> (Vec3, Vec3) {
        let inv_scale = 1.0 / 1024.0;
        let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (x, y, z) in QUAKE_E1M1_VERTS {
            let wx = *x as f32 * inv_scale;
            let wy = *y as f32 * inv_scale;
            let wz = *z as f32 * inv_scale;
            min.x = min.x.min(wx);
            min.y = min.y.min(wy);
            min.z = min.z.min(wz);
            max.x = max.x.max(wx);
            max.y = max.y.max(wy);
            max.z = max.z.max(wz);
        }
        (min, max)
    }

    fn build_collision() -> (Vec<WallSeg>, Vec<FloorTri>) {
        let inv_scale = 1.0 / 1024.0;
        let mut walls = Vec::new();
        let mut floors = Vec::new();

        let mut push_edge = |a: Vec3, b: Vec3| {
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            if (dx * dx + dy * dy) <= 1e-6 {
                return;
            }
            walls.push(WallSeg {
                x1: a.x,
                y1: a.y,
                x2: b.x,
                y2: b.y,
            });
        };

        for (i0, i1, i2) in QUAKE_E1M1_TRIS.iter().copied() {
            let v0 = QUAKE_E1M1_VERTS[i0 as usize];
            let v1 = QUAKE_E1M1_VERTS[i1 as usize];
            let v2 = QUAKE_E1M1_VERTS[i2 as usize];

            let w0 = Vec3::new(
                v0.0 as f32 * inv_scale,
                v0.1 as f32 * inv_scale,
                v0.2 as f32 * inv_scale,
            );
            let w1 = Vec3::new(
                v1.0 as f32 * inv_scale,
                v1.1 as f32 * inv_scale,
                v1.2 as f32 * inv_scale,
            );
            let w2 = Vec3::new(
                v2.0 as f32 * inv_scale,
                v2.1 as f32 * inv_scale,
                v2.2 as f32 * inv_scale,
            );

            let n = (w1 - w0).cross(w2 - w0);
            let len = n.len();
            if len <= 1e-6 {
                continue;
            }
            // let n = n * (1.0 / len); // normalize

            if n.z.abs() < 0.35 {
                push_edge(w0, w1);
                push_edge(w1, w2);
                push_edge(w2, w0);
            }

            if n.z > 0.35
                && let Some(tri) = FloorTri::new(w0, w1, w2)
            {
                floors.push(tri);
            }
        }

        (walls, floors)
    }

    fn ground_height_at(&self, x: f32, y: f32) -> Option<f32> {
        let mut best = None;
        let eps = 1e-3;

        for tri in &self.floor_tris {
            if x < tri.min_x || x > tri.max_x || y < tri.min_y || y > tri.max_y {
                continue;
            }

            let w0 = cross2(tri.v1.x - x, tri.v1.y - y, tri.v2.x - x, tri.v2.y - y) / tri.area;
            let w1 = cross2(tri.v2.x - x, tri.v2.y - y, tri.v0.x - x, tri.v0.y - y) / tri.area;
            let w2 = 1.0 - w0 - w1;

            if w0 >= -eps && w1 >= -eps && w2 >= -eps {
                let z = w0 * tri.v0.z + w1 * tri.v1.z + w2 * tri.v2.z;
                if best.is_none_or(|best_z| z > best_z) {
                    best = Some(z);
                }
            }
        }

        best
    }

    fn ground_eye_height(&self, x: f32, y: f32) -> f32 {
        let ground = self.ground_height_at(x, y).unwrap_or(self.bounds_min.z);
        ground + QUAKE_EYE_HEIGHT
    }

    fn collides(&self, x: f32, y: f32) -> bool {
        let radius_sq = QUAKE_COLLISION_RADIUS * QUAKE_COLLISION_RADIUS;
        for seg in &self.wall_segments {
            let dist_sq = point_segment_distance_sq(x, y, seg.x1, seg.y1, seg.x2, seg.y2);
            if dist_sq < radius_sq {
                return true;
            }
        }
        false
    }

    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.player.yaw = (self.player.yaw + yaw_delta) % TAU;
        self.player.pitch = (self.player.pitch + pitch_delta).clamp(-1.2, 1.2);
    }

    pub fn set_move_fwd(&mut self, val: f32) {
        self.move_fwd = val.clamp(-1.0, 1.0);
    }

    pub fn set_move_side(&mut self, val: f32) {
        self.move_side = val.clamp(-1.0, 1.0);
    }

    pub fn jump(&mut self) {
        if self.player.grounded {
            self.player.vel.z = QUAKE_JUMP_VELOCITY;
            self.player.grounded = false;
        }
    }

    pub fn fire(&mut self) {
        self.fire_flash = 1.0;
    }

    fn apply_physics(&mut self) {
        let (sy, cy) = self.player.yaw.sin_cos();
        let fwd_x = cy;
        let fwd_y = sy;
        let side_x = -sy;
        let side_y = cy;

        let target_vx = (fwd_x * self.move_fwd * QUAKE_MOVE_SPEED)
            + (side_x * self.move_side * QUAKE_STRAFE_SPEED);
        let target_vy = (fwd_y * self.move_fwd * QUAKE_MOVE_SPEED)
            + (side_y * self.move_side * QUAKE_STRAFE_SPEED);

        self.player.vel.x += (target_vx - self.player.vel.x) * 0.2;
        self.player.vel.y += (target_vy - self.player.vel.y) * 0.2;

        self.player.vel.x *= QUAKE_FRICTION;
        self.player.vel.y *= QUAKE_FRICTION;

        let mut nx = self.player.pos.x + self.player.vel.x;
        let mut ny = self.player.pos.y;
        if self.collides(nx, ny) {
            self.player.vel.x = 0.0;
        } else {
            self.player.pos.x = nx;
        }

        nx = self.player.pos.x;
        ny = self.player.pos.y + self.player.vel.y;
        if self.collides(nx, ny) {
            self.player.vel.y = 0.0;
        } else {
            self.player.pos.y = ny;
        }

        let margin = (QUAKE_COLLISION_RADIUS + 0.02).max(0.04);
        let min_x = self.bounds_min.x + margin;
        let max_x = self.bounds_max.x - margin;
        let min_y = self.bounds_min.y + margin;
        let max_y = self.bounds_max.y - margin;
        self.player.pos.x = self.player.pos.x.clamp(min_x, max_x);
        self.player.pos.y = self.player.pos.y.clamp(min_y, max_y);

        if !self.player.grounded {
            self.player.vel.z += QUAKE_GRAVITY * 0.05;
            self.player.pos.z += self.player.vel.z;
        }

        let ground = self.ground_eye_height(self.player.pos.x, self.player.pos.y);

        if self.player.pos.z <= ground {
            self.player.pos.z = ground;
            self.player.vel.z = 0.0;
            self.player.grounded = true;
        } else if self.player.pos.z > ground + 0.05 {
            self.player.grounded = false;
        }
    }

    pub fn update(&mut self) {
        if self.fire_flash > 0.0 {
            self.fire_flash = (self.fire_flash - 0.1).max(0.0);
        }
        self.apply_physics();
    }

    fn ensure_depth(&mut self, width: u16, height: u16) {
        let len = width as usize * height as usize;
        if len > self.depth.len() {
            self.depth.resize(len, f32::INFINITY);
        }
        self.depth_w = width;
        self.depth_h = height;
    }

    fn clear_depth(&mut self) {
        let len = self.depth_w as usize * self.depth_h as usize;
        if len > 0 {
            self.depth[..len].fill(f32::INFINITY);
        }
    }

    #[cfg(test)]
    pub fn player(&self) -> &QuakePlayer {
        &self.player
    }

    pub fn render(
        &mut self,
        painter: &mut Painter,
        width: u16,
        height: u16,
        quality: FxQuality,
        _time: f64,
        frame: u64,
    ) {
        if width == 0 || height == 0 {
            return;
        }

        let stride = match quality {
            FxQuality::Off => 0,
            _ => 1,
        };
        if stride == 0 {
            return;
        }

        self.ensure_depth(width, height);
        self.clear_depth();

        let w = width as f32;
        let h = height as f32;
        let center = Vec3::new(w * 0.5, h * 0.5, 0.0);
        let eye = self.player.pos;

        let (sy, cy) = self.player.yaw.sin_cos();
        let (sp, cp) = self.player.pitch.sin_cos();
        let forward = Vec3::new(cy * cp, sy * cp, sp).normalized();
        let right = Vec3::new(-sy, cy, 0.0).normalized();
        let up = right.cross(forward).normalized();

        let light_dir = Vec3::new(0.4, -0.6, 0.5).normalized();

        let proj_scale = w.min(h) * 0.9;
        let near = 0.04f32;
        let far = 8.0f32;

        let inv_scale = 1.0 / 1024.0;
        let tri_step = match quality {
            FxQuality::Off => 0,
            _ => 1,
        };
        let edge_stride = if tri_step > 1 { tri_step * 2 } else { 1 };

        let edge = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
            (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
        };

        for (tri_idx, tri) in QUAKE_E1M1_TRIS.iter().enumerate().step_by(tri_step) {
            let (i0, i1, i2) = (tri.0 as usize, tri.1 as usize, tri.2 as usize);
            let v0 = QUAKE_E1M1_VERTS[i0];
            let v1 = QUAKE_E1M1_VERTS[i1];
            let v2 = QUAKE_E1M1_VERTS[i2];

            let w0 = Vec3::new(
                v0.0 as f32 * inv_scale,
                v0.1 as f32 * inv_scale,
                v0.2 as f32 * inv_scale,
            );
            let w1 = Vec3::new(
                v1.0 as f32 * inv_scale,
                v1.1 as f32 * inv_scale,
                v1.2 as f32 * inv_scale,
            );
            let w2 = Vec3::new(
                v2.0 as f32 * inv_scale,
                v2.1 as f32 * inv_scale,
                v2.2 as f32 * inv_scale,
            );

            let n = (w1 - w0).cross(w2 - w0).normalized();
            let view_dir = (eye - w0).normalized();
            let facing = n.dot(view_dir);
            if facing <= 0.02 {
                continue;
            }
            let diffuse = n.dot(light_dir).max(0.0);
            let rim = (1.0 - facing.clamp(0.0, 1.0)).powf(3.0) * 0.5;

            let height_span = (self.bounds_max.z - self.bounds_min.z).max(0.001);
            let height_t = ((w0.z - self.bounds_min.z) / height_span).clamp(0.0, 1.0);
            let base = palette_quake_stone(height_t as f64);
            let ambient = 0.15f32;
            let light = (ambient + diffuse * 0.8 + rim).clamp(0.0, 1.5);

            let cam0 = Vec3::new(
                (w0 - eye).dot(right),
                (w0 - eye).dot(up),
                (w0 - eye).dot(forward),
            );
            let cam1 = Vec3::new(
                (w1 - eye).dot(right),
                (w1 - eye).dot(up),
                (w1 - eye).dot(forward),
            );
            let cam2 = Vec3::new(
                (w2 - eye).dot(right),
                (w2 - eye).dot(up),
                (w2 - eye).dot(forward),
            );
            let clipped = clip_polygon_near(&[cam0, cam1, cam2], near);
            if clipped.len() < 3 {
                continue;
            }

            let mut draw_tri = |a: Vec3, b: Vec3, c: Vec3| {
                let sx0 = center.x + (a.x / a.z) * proj_scale;
                let sy0 = center.y - (a.y / a.z) * proj_scale;
                let sx1 = center.x + (b.x / b.z) * proj_scale;
                let sy1 = center.y - (b.y / b.z) * proj_scale;
                let sx2 = center.x + (c.x / c.z) * proj_scale;
                let sy2 = center.y - (c.y / c.z) * proj_scale;

                let minx = sx0.min(sx1).min(sx2).floor().max(0.0) as i32;
                let maxx = sx0.max(sx1).max(sx2).ceil().min(w - 1.0) as i32;
                let miny = sy0.min(sy1).min(sy2).floor().max(0.0) as i32;
                let maxy = sy0.max(sy1).max(sy2).ceil().min(h - 1.0) as i32;

                if minx > maxx || miny > maxy {
                    return;
                }

                let area = edge(sx0, sy0, sx1, sy1, sx2, sy2);
                if area.abs() < 1e-5 {
                    return;
                }

                let inv_area = 1.0 / area;
                let stride_usize = stride;

                for py in (miny..=maxy).step_by(stride_usize) {
                    let fy = py as f32;
                    for px in (minx..=maxx).step_by(stride_usize) {
                        let fx = px as f32;
                        let w0e = edge(sx1, sy1, sx2, sy2, fx, fy);
                        let w1e = edge(sx2, sy2, sx0, sy0, fx, fy);
                        let w2e = edge(sx0, sy0, sx1, sy1, fx, fy);

                        if (w0e * area) < 0.0 || (w1e * area) < 0.0 || (w2e * area) < 0.0 {
                            continue;
                        }

                        let b0 = w0e * inv_area;
                        let b1 = w1e * inv_area;
                        let b2 = w2e * inv_area;
                        let z = b0 * a.z + b1 * b.z + b2 * c.z;

                        let idx = py as usize * width as usize + px as usize;
                        if z >= self.depth[idx] {
                            continue;
                        }
                        self.depth[idx] = z;

                        let fog = ((z - near) / (far - near)).clamp(0.0, 1.0);
                        let fade = (1.0 - fog).powf(1.8);
                        let grain = (((px as u64).wrapping_mul(73856093)
                            ^ (py as u64).wrapping_mul(19349663)
                            ^ frame)
                            & 3) as f32
                            / 12.0;
                        let mut brightness = (light * fade + grain).clamp(0.0, 1.0);
                        if self.fire_flash > 0.0 {
                            brightness = (brightness + self.fire_flash * 0.4).min(1.3);
                        }

                        let r = (base.r() as f32 * brightness) as u8;
                        let g = (base.g() as f32 * brightness) as u8;
                        let b = (base.b() as f32 * brightness) as u8;
                        painter.point_colored(px, py, PackedRgba::rgb(r, g, b));
                    }
                }

                if tri_idx % edge_stride == 0 {
                    let edge_boost = (light + 0.4).clamp(0.0, 1.4);
                    let er = (base.r() as f32 * edge_boost).min(255.0) as u8;
                    let eg = (base.g() as f32 * edge_boost).min(255.0) as u8;
                    let eb = (base.b() as f32 * edge_boost).min(255.0) as u8;
                    let edge_color = PackedRgba::rgb(er, eg, eb);

                    painter.line_colored(
                        sx0 as i32,
                        sy0 as i32,
                        sx1 as i32,
                        sy1 as i32,
                        Some(edge_color),
                    );
                    painter.line_colored(
                        sx1 as i32,
                        sy1 as i32,
                        sx2 as i32,
                        sy2 as i32,
                        Some(edge_color),
                    );
                    painter.line_colored(
                        sx2 as i32,
                        sy2 as i32,
                        sx0 as i32,
                        sy0 as i32,
                        Some(edge_color),
                    );
                }
            };

            if clipped.len() == 3 {
                draw_tri(clipped[0], clipped[1], clipped[2]);
            } else {
                for i in 1..(clipped.len() - 1) {
                    draw_tri(clipped[0], clipped[i], clipped[i + 1]);
                }
            }
        }

        let cx = (width / 2) as i32;
        let cy = (height / 2) as i32;
        let flash = self.fire_flash;
        let cross_r = (200.0 + flash * 55.0).min(255.0) as u8;
        let cross_g = (240.0 - flash * 100.0).max(0.0) as u8;
        let cross = PackedRgba::rgb(cross_r, cross_g, cross_g);

        painter.line_colored(cx - 4, cy, cx - 2, cy, Some(cross));
        painter.line_colored(cx + 2, cy, cx + 4, cy, Some(cross));
        painter.line_colored(cx, cy - 4, cx, cy - 2, Some(cross));
        painter.line_colored(cx, cy + 2, cx, cy + 4, Some(cross));
        painter.point_colored(cx, cy, PackedRgba::rgb(255, 50, 50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    // ── Vec3 math tests ──────────────────────────────────────────────

    #[test]
    fn vec3_new_and_fields() {
        let v = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(v.x, 1.0);
        assert_eq!(v.y, 2.0);
        assert_eq!(v.z, 3.0);
    }

    #[test]
    fn vec3_add() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        let c = a + b;
        assert!((c.x - 5.0).abs() < EPS);
        assert!((c.y - 7.0).abs() < EPS);
        assert!((c.z - 9.0).abs() < EPS);
    }

    #[test]
    fn vec3_sub() {
        let a = Vec3::new(5.0, 7.0, 9.0);
        let b = Vec3::new(1.0, 2.0, 3.0);
        let c = a - b;
        assert!((c.x - 4.0).abs() < EPS);
        assert!((c.y - 5.0).abs() < EPS);
        assert!((c.z - 6.0).abs() < EPS);
    }

    #[test]
    fn vec3_mul_scalar() {
        let v = Vec3::new(1.0, 2.0, 3.0);
        let s = v * 2.0;
        assert!((s.x - 2.0).abs() < EPS);
        assert!((s.y - 4.0).abs() < EPS);
        assert!((s.z - 6.0).abs() < EPS);
    }

    #[test]
    fn vec3_dot() {
        let a = Vec3::new(1.0, 0.0, 0.0);
        let b = Vec3::new(0.0, 1.0, 0.0);
        assert!(
            (a.dot(b)).abs() < EPS,
            "orthogonal vectors dot product should be 0"
        );
        assert!(
            (a.dot(a) - 1.0).abs() < EPS,
            "unit vector dot self should be 1"
        );
    }

    #[test]
    fn vec3_dot_general() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        // 1*4 + 2*5 + 3*6 = 32
        assert!((a.dot(b) - 32.0).abs() < EPS);
    }

    #[test]
    fn vec3_cross_basis() {
        let x = Vec3::new(1.0, 0.0, 0.0);
        let y = Vec3::new(0.0, 1.0, 0.0);
        let z = x.cross(y);
        assert!((z.x).abs() < EPS);
        assert!((z.y).abs() < EPS);
        assert!((z.z - 1.0).abs() < EPS, "x cross y should be z");
    }

    #[test]
    fn vec3_cross_anticommutative() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        let ab = a.cross(b);
        let ba = b.cross(a);
        assert!((ab.x + ba.x).abs() < EPS);
        assert!((ab.y + ba.y).abs() < EPS);
        assert!((ab.z + ba.z).abs() < EPS);
    }

    #[test]
    fn vec3_len() {
        let v = Vec3::new(3.0, 4.0, 0.0);
        assert!((v.len() - 5.0).abs() < EPS);
    }

    #[test]
    fn vec3_len_zero() {
        let v = Vec3::new(0.0, 0.0, 0.0);
        assert!(v.len().abs() < EPS);
    }

    #[test]
    fn vec3_normalized_unit() {
        let v = Vec3::new(3.0, 4.0, 0.0);
        let n = v.normalized();
        assert!(
            (n.len() - 1.0).abs() < EPS,
            "normalized should have length 1"
        );
        assert!((n.x - 0.6).abs() < EPS);
        assert!((n.y - 0.8).abs() < EPS);
    }

    #[test]
    fn vec3_normalized_zero_returns_self() {
        let v = Vec3::new(0.0, 0.0, 0.0);
        let n = v.normalized();
        assert!(n.x.abs() < EPS);
        assert!(n.y.abs() < EPS);
        assert!(n.z.abs() < EPS);
    }

    // ── cross2 tests ─────────────────────────────────────────────────

    #[test]
    fn cross2_zero_for_parallel() {
        assert!((cross2(1.0, 0.0, 2.0, 0.0)).abs() < EPS);
    }

    #[test]
    fn cross2_positive_ccw() {
        // x-axis cross y-axis should be positive (CCW)
        assert!(cross2(1.0, 0.0, 0.0, 1.0) > 0.0);
    }

    #[test]
    fn cross2_negative_cw() {
        assert!(cross2(0.0, 1.0, 1.0, 0.0) < 0.0);
    }

    // ── point_segment_distance_sq tests ──────────────────────────────

    #[test]
    fn distance_to_segment_at_endpoint() {
        // Point is closest to the start of a segment
        let dist = point_segment_distance_sq(0.0, 0.0, 1.0, 0.0, 2.0, 0.0);
        assert!((dist - 1.0).abs() < EPS, "distance to start should be 1.0");
    }

    #[test]
    fn distance_to_segment_perpendicular() {
        // Point (0, 1) perpendicular to segment (0,0)-(2,0), closest at (0,0)
        let dist = point_segment_distance_sq(0.0, 1.0, 0.0, 0.0, 2.0, 0.0);
        assert!((dist - 1.0).abs() < EPS);
    }

    #[test]
    fn distance_to_segment_midpoint() {
        // Point (1, 1) above segment (0,0)-(2,0), closest at (1,0)
        let dist = point_segment_distance_sq(1.0, 1.0, 0.0, 0.0, 2.0, 0.0);
        assert!((dist - 1.0).abs() < EPS);
    }

    #[test]
    fn distance_to_degenerate_segment() {
        // Degenerate segment (point)
        let dist = point_segment_distance_sq(3.0, 4.0, 0.0, 0.0, 0.0, 0.0);
        assert!((dist - 25.0).abs() < EPS, "distance to point segment");
    }

    // ── clip_polygon_near tests ──────────────────────────────────────

    #[test]
    fn clip_empty_polygon() {
        let result = clip_polygon_near(&[], 1.0);
        assert!(result.is_empty());
    }

    #[test]
    fn clip_all_in_front() {
        let poly = [
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(1.0, 0.0, 2.0),
            Vec3::new(0.0, 1.0, 2.0),
        ];
        let result = clip_polygon_near(&poly, 1.0);
        assert_eq!(result.len(), 3, "all-in-front triangle should be unclipped");
    }

    #[test]
    fn clip_all_behind() {
        let poly = [
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(1.0, 0.0, 0.1),
            Vec3::new(0.0, 1.0, 0.1),
        ];
        let result = clip_polygon_near(&poly, 1.0);
        assert!(
            result.is_empty(),
            "all-behind triangle should be fully clipped"
        );
    }

    #[test]
    fn clip_partial_produces_valid_polygon() {
        // One vertex behind, two in front
        let poly = [
            Vec3::new(0.0, 0.0, 0.5), // Behind near=1.0
            Vec3::new(1.0, 0.0, 2.0), // In front
            Vec3::new(0.0, 1.0, 2.0), // In front
        ];
        let result = clip_polygon_near(&poly, 1.0);
        assert!(
            result.len() >= 3,
            "partial clip should produce >= 3 vertices"
        );
        // All clipped vertices must be at or beyond the near plane
        for v in &result {
            assert!(
                v.z >= 1.0 - EPS,
                "clipped vertex z={} should be >= near=1.0",
                v.z
            );
        }
    }

    // ── palette_quake_stone tests ────────────────────────────────────

    #[test]
    fn palette_at_zero() {
        let c = palette_quake_stone(0.0);
        // Should be the dark mud color (47, 43, 35)
        assert_eq!(c.r(), 47);
        assert_eq!(c.g(), 43);
        assert_eq!(c.b(), 35);
    }

    #[test]
    fn palette_at_one() {
        let c = palette_quake_stone(1.0);
        // Should be grey stone (110, 100, 90)
        assert_eq!(c.r(), 110);
        assert_eq!(c.g(), 100);
        assert_eq!(c.b(), 90);
    }

    #[test]
    fn palette_clamps_out_of_range() {
        // Values beyond [0, 1] should clamp
        let c_neg = palette_quake_stone(-1.0);
        let c_zero = palette_quake_stone(0.0);
        assert_eq!(c_neg.r(), c_zero.r());
        assert_eq!(c_neg.g(), c_zero.g());

        let c_over = palette_quake_stone(2.0);
        let c_one = palette_quake_stone(1.0);
        assert_eq!(c_over.r(), c_one.r());
        assert_eq!(c_over.g(), c_one.g());
    }

    // ── FloorTri tests ───────────────────────────────────────────────

    #[test]
    fn floor_tri_degenerate_returns_none() {
        // Three collinear points should produce None (zero area)
        let v0 = Vec3::new(0.0, 0.0, 0.0);
        let v1 = Vec3::new(1.0, 0.0, 0.0);
        let v2 = Vec3::new(2.0, 0.0, 0.0);
        assert!(FloorTri::new(v0, v1, v2).is_none());
    }

    #[test]
    fn floor_tri_valid_computes_bounds() {
        let v0 = Vec3::new(0.0, 0.0, 1.0);
        let v1 = Vec3::new(1.0, 0.0, 1.0);
        let v2 = Vec3::new(0.0, 1.0, 1.0);
        let tri = FloorTri::new(v0, v1, v2).expect("valid triangle");
        assert!((tri.min_x - 0.0).abs() < EPS);
        assert!((tri.max_x - 1.0).abs() < EPS);
        assert!((tri.min_y - 0.0).abs() < EPS);
        assert!((tri.max_y - 1.0).abs() < EPS);
        assert!(tri.area.abs() > EPS, "area should be nonzero");
    }

    // ── QuakePlayer tests ────────────────────────────────────────────

    #[test]
    fn player_starts_grounded() {
        let player = QuakePlayer::new(Vec3::new(0.0, 0.0, 0.0));
        assert!(player.grounded);
        assert!((player.vel.x).abs() < EPS);
        assert!((player.vel.y).abs() < EPS);
        assert!((player.vel.z).abs() < EPS);
    }

    // ── QuakeE1M1State tests ─────────────────────────────────────────

    #[test]
    fn state_default_constructs() {
        let state = QuakeE1M1State::default();
        assert!(state.player.grounded);
        assert!(!state.wall_segments.is_empty(), "should have wall segments");
        // floor_tris may be empty depending on geometry normals
        assert!(state.player.pos.x.is_finite());
        assert!(state.player.pos.y.is_finite());
        assert!(state.player.pos.z.is_finite());
    }

    #[test]
    fn state_look_clamps_pitch() {
        let mut state = QuakeE1M1State::default();
        state.look(0.0, 10.0);
        assert!(state.player.pitch <= 1.2 + EPS, "pitch should be clamped");
        state.look(0.0, -20.0);
        assert!(state.player.pitch >= -1.2 - EPS, "pitch should be clamped");
    }

    #[test]
    fn state_look_wraps_yaw() {
        let mut state = QuakeE1M1State::default();
        state.look(TAU + 0.1, 0.0);
        assert!(state.player.yaw < TAU, "yaw should wrap around TAU");
    }

    #[test]
    fn state_set_move_fwd_clamps() {
        let mut state = QuakeE1M1State::default();
        state.set_move_fwd(5.0);
        assert!((state.move_fwd - 1.0).abs() < EPS);
        state.set_move_fwd(-5.0);
        assert!((state.move_fwd + 1.0).abs() < EPS);
    }

    #[test]
    fn state_set_move_side_clamps() {
        let mut state = QuakeE1M1State::default();
        state.set_move_side(5.0);
        assert!((state.move_side - 1.0).abs() < EPS);
    }

    #[test]
    fn state_jump_sets_velocity() {
        let mut state = QuakeE1M1State::default();
        assert!(state.player.grounded);
        state.jump();
        assert!(!state.player.grounded);
        assert!((state.player.vel.z - QUAKE_JUMP_VELOCITY).abs() < EPS);
    }

    #[test]
    fn state_jump_no_double_jump() {
        let mut state = QuakeE1M1State::default();
        state.jump();
        state.player.vel.z = 0.1; // Simulate mid-air
        state.jump(); // Should be a no-op since not grounded
        assert!(
            (state.player.vel.z - 0.1).abs() < EPS,
            "should not double jump"
        );
    }

    #[test]
    fn state_fire_sets_flash() {
        let mut state = QuakeE1M1State::default();
        state.fire();
        assert!((state.fire_flash - 1.0).abs() < EPS);
    }

    #[test]
    fn state_update_decays_fire_flash() {
        let mut state = QuakeE1M1State::default();
        state.fire();
        state.update();
        assert!(state.fire_flash < 1.0, "fire flash should decay");
        for _ in 0..20 {
            state.update();
        }
        assert!(
            state.fire_flash.abs() < EPS,
            "fire flash should reach 0 eventually"
        );
    }

    #[test]
    fn state_update_does_not_panic() {
        let mut state = QuakeE1M1State::default();
        state.set_move_fwd(1.0);
        state.set_move_side(0.5);
        for _ in 0..100 {
            state.update();
        }
        // Player should still be within bounds
        assert!(state.player.pos.x.is_finite());
        assert!(state.player.pos.y.is_finite());
        assert!(state.player.pos.z.is_finite());
    }

    #[test]
    fn state_player_stays_within_bounds_after_movement() {
        let mut state = QuakeE1M1State::default();
        let margin = (QUAKE_COLLISION_RADIUS + 0.02).max(0.04);
        state.set_move_fwd(1.0);
        for _ in 0..500 {
            state.update();
        }
        assert!(state.player.pos.x >= state.bounds_min.x + margin - EPS);
        assert!(state.player.pos.x <= state.bounds_max.x - margin + EPS);
        assert!(state.player.pos.y >= state.bounds_min.y + margin - EPS);
        assert!(state.player.pos.y <= state.bounds_max.y - margin + EPS);
    }
}
