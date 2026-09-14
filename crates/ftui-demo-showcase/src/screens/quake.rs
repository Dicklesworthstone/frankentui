use std::cell::RefCell;
use std::f32::consts::TAU;

use ftui_core::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::canvas::{Canvas, Mode, Painter};
use ftui_extras::visual_fx::FxQuality;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_widgets::Widget;
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

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

// The level is authored in Quake units and scaled by 1/1024, so the player has
// to be scaled the same way or nothing fits: id's player is 32 units wide and
// 56 tall, which is 0.031 x 0.055 here. At the old 0.12-wide body the player
// was twelve feet across and no corridor on the map would admit them.
const QUAKE_EYE_HEIGHT: f32 = 0.045;
const QUAKE_GRAVITY: f32 = -0.156;
const QUAKE_JUMP_VELOCITY: f32 = 0.026;
const QUAKE_COLLISION_RADIUS: f32 = 0.016;
/// How far above the feet the body reaches for collision purposes.
const QUAKE_BODY_HEIGHT: f32 = 0.055;
/// Ledges no taller than this are walked over rather than bumped into.
const QUAKE_STEP_HEIGHT: f32 = 0.018;
/// Playable inset from the level bounding box.
const QUAKE_PLAY_MARGIN: f32 = 0.02;
/// Largest distance moved between collision tests, as a fraction of the body
/// radius. A whole tick of running covers more ground than the body is wide,
/// and a single test at the destination would step clean through a thin wall.
const QUAKE_SUBSTEP_FRACTION: f32 = 0.5;
/// Cap on substeps per tick, so a wild velocity cannot stall the frame.
///
/// Top speed needs eight; the headroom is so that nudging the move speed does
/// not silently reintroduce tunnelling. See `the_substep_cap_covers_top_speed`.
const QUAKE_MAX_SUBSTEPS: usize = 12;
/// How many of the widest floors to try before giving up on a spawn point.
const QUAKE_SPAWN_CANDIDATES: usize = 24;
/// Headings sampled when choosing which way a fresh spawn faces.
const QUAKE_SPAWN_YAW_STEPS: usize = 16;
/// How far ahead a spawn candidate's sightline is probed, in body radii.
/// Long enough to tell a hall from an alcove: 40 radii is half the map.
const QUAKE_SPAWN_PROBE_STEPS: usize = 40;
/// Side of a collision-grid cell, in world units.
const QUAKE_GRID_CELL: f32 = 0.03;
/// Camera near plane.
///
/// Must stay inside everything the player can physically reach: collision stops
/// the body one radius from a wall and the eye sits `QUAKE_EYE_HEIGHT` above the
/// floor, so a near plane beyond either of those clips away the surface you are
/// standing against and leaves a hole in the middle of the view.
const QUAKE_NEAR_PLANE: f32 = QUAKE_COLLISION_RADIUS * 0.5;
/// How far the player's own light reaches, in world units.
///
/// The scene is lit by one fixed directional light plus a little ambient, so a
/// wall facing away from it is nearly black. That never showed before, because
/// the body was wide enough to keep such a wall outside the near plane; now the
/// player can stand a finger's width from one and it fills the view. A light
/// that travels with the camera is both the fix and the depth cue this flat
/// little renderer was missing.
const QUAKE_LAMP_RANGE: f32 = 0.35;
/// How much the player's light adds at point-blank range.
const QUAKE_LAMP_GAIN: f32 = 0.55;

// Relationships the rest of the file assumes, checked where they are written
// rather than in a test that can only report them after the fact.
const _: () = assert!(QUAKE_NEAR_PLANE < QUAKE_COLLISION_RADIUS);
const _: () = assert!(QUAKE_NEAR_PLANE < QUAKE_EYE_HEIGHT);
// The playable inset has to clear the body, or the bounds themselves collide.
const _: () = assert!(QUAKE_PLAY_MARGIN >= QUAKE_COLLISION_RADIUS);
const QUAKE_MOVE_SPEED: f32 = 0.08;
const QUAKE_STRAFE_SPEED: f32 = 0.07;
const QUAKE_FRICTION: f32 = 0.85;

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

#[derive(Debug, Clone, Copy)]
struct ClippedTriangle {
    verts: [Vec3; 4],
    len: usize,
}

impl ClippedTriangle {
    fn new() -> Self {
        Self {
            verts: [Vec3::new(0.0, 0.0, 0.0); 4],
            len: 0,
        }
    }

    fn push(&mut self, vertex: Vec3) {
        debug_assert!(self.len < self.verts.len());
        if self.len < self.verts.len() {
            self.verts[self.len] = vertex;
            self.len += 1;
        }
    }

    fn as_slice(&self) -> &[Vec3] {
        &self.verts[..self.len]
    }

    fn len(&self) -> usize {
        self.len
    }
}

fn clip_triangle_near(poly: [Vec3; 3], near: f32) -> ClippedTriangle {
    let mut out = ClippedTriangle::new();
    let mut prev = poly[2];
    let mut prev_inside = prev.z >= near;

    for curr in poly {
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
    /// Vertical extent of the triangle this edge came from.
    ///
    /// Collision is solved in 2D, but the level is not flat: rooms sit above
    /// rooms. Without the span, every wall anywhere in the map blocks the
    /// floor beneath it and there is nowhere left to stand.
    z_min: f32,
    z_max: f32,
}

/// Uniform grid over the level footprint, mapping a cell to the wall segments
/// that touch it.
///
/// E1M1 is 18k segments once the vertical faces are collected. Scanning all of
/// them twice a tick is wasteful in a browser, and the spawn search - which
/// probes hundreds of points - would be unusable without this.
#[derive(Debug, Clone)]
struct WallGrid {
    min_x: f32,
    min_y: f32,
    cell: f32,
    cols: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
}

impl WallGrid {
    fn new(walls: &[WallSeg], min: Vec3, max: Vec3) -> Self {
        let cell = QUAKE_GRID_CELL;
        let cols = (((max.x - min.x) / cell).ceil() as usize).max(1);
        let rows = (((max.y - min.y) / cell).ceil() as usize).max(1);
        let mut grid = Self {
            min_x: min.x,
            min_y: min.y,
            cell,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
        };
        for (i, seg) in walls.iter().enumerate() {
            let (x0, x1, y0, y1) = grid.span(
                seg.x1.min(seg.x2),
                seg.x1.max(seg.x2),
                seg.y1.min(seg.y2),
                seg.y1.max(seg.y2),
            );
            for gy in y0..=y1 {
                for gx in x0..=x1 {
                    grid.cells[gy * cols + gx].push(i as u32);
                }
            }
        }
        grid
    }

    /// Cell range covering a bounding box, clamped to the grid.
    fn span(&self, min_x: f32, max_x: f32, min_y: f32, max_y: f32) -> (usize, usize, usize, usize) {
        let to_col =
            |v: f32| (((v - self.min_x) / self.cell).floor().max(0.0) as usize).min(self.cols - 1);
        let to_row =
            |v: f32| (((v - self.min_y) / self.cell).floor().max(0.0) as usize).min(self.rows - 1);
        (to_col(min_x), to_col(max_x), to_row(min_y), to_row(max_y))
    }

    /// Wall indices that could be within `radius` of `(x, y)`.
    fn near(&self, x: f32, y: f32, radius: f32) -> impl Iterator<Item = u32> + '_ {
        let (x0, x1, y0, y1) = self.span(x - radius, x + radius, y - radius, y + radius);
        (y0..=y1)
            .flat_map(move |gy| (x0..=x1).map(move |gx| gy * self.cols + gx))
            .flat_map(move |i| self.cells[i].iter().copied())
    }
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

#[derive(Debug, Clone, Copy)]
struct RenderTri {
    i0: usize,
    i1: usize,
    i2: usize,
    normal: Vec3,
    diffuse: f32,
    base: PackedRgba,
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
    wall_grid: WallGrid,
    floor_tris: Vec<FloorTri>,
    world_vertices: Vec<Vec3>,
    camera_vertices: Vec<Vec3>,
    render_tris: Vec<RenderTri>,
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
        let wall_grid = WallGrid::new(&wall_segments, min, max);
        let (world_vertices, render_tris) = Self::build_render_mesh(min, max);
        let camera_vertices = vec![Vec3::new(0.0, 0.0, 0.0); world_vertices.len()];
        let start = Vec3::new(
            (min.x + max.x) * 0.5,
            (min.y + max.y) * 0.5,
            min.z + QUAKE_EYE_HEIGHT,
        );
        let mut state = Self {
            player: QuakePlayer::new(start),
            fire_flash: 0.0,
            bounds_min: min,
            bounds_max: max,
            wall_segments,
            wall_grid,
            floor_tris,
            world_vertices,
            camera_vertices,
            render_tris,
            depth: Vec::new(),
            depth_w: 0,
            depth_h: 0,
            move_fwd: 0.0,
            move_side: 0.0,
        };
        state.respawn();
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

        let mut push_edge = |a: Vec3, b: Vec3, z_min: f32, z_max: f32| {
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
                z_min,
                z_max,
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
            // Classify on the unit normal. The raw cross product scales with
            // triangle area, and at this mesh's scale that is around 1e-6, so
            // an un-normalized `n.z` compared against 0.35 calls every single
            // triangle a wall: no floors, and a player sealed in place.
            let n = n * (1.0 / len);
            let z_min = w0.z.min(w1.z).min(w2.z);
            let z_max = w0.z.max(w1.z).max(w2.z);

            if n.z.abs() < 0.35 {
                push_edge(w0, w1, z_min, z_max);
                push_edge(w1, w2, z_min, z_max);
                push_edge(w2, w0, z_min, z_max);
            }

            if n.z > 0.35
                && let Some(tri) = FloorTri::new(w0, w1, w2)
            {
                floors.push(tri);
            }
        }

        (walls, floors)
    }

    fn build_render_mesh(bounds_min: Vec3, bounds_max: Vec3) -> (Vec<Vec3>, Vec<RenderTri>) {
        let inv_scale = 1.0 / 1024.0;
        let world_vertices = QUAKE_E1M1_VERTS
            .iter()
            .map(|(x, y, z)| {
                Vec3::new(
                    *x as f32 * inv_scale,
                    *y as f32 * inv_scale,
                    *z as f32 * inv_scale,
                )
            })
            .collect::<Vec<_>>();
        let height_span = (bounds_max.z - bounds_min.z).max(0.001);
        let light_dir = Vec3::new(0.4, -0.6, 0.5).normalized();
        let render_tris = QUAKE_E1M1_TRIS
            .iter()
            .copied()
            .map(|(i0, i1, i2)| {
                let i0 = i0 as usize;
                let i1 = i1 as usize;
                let i2 = i2 as usize;
                let w0 = world_vertices[i0];
                let w1 = world_vertices[i1];
                let w2 = world_vertices[i2];
                let normal = (w1 - w0).cross(w2 - w0).normalized();
                let height_t = ((w0.z - bounds_min.z) / height_span).clamp(0.0, 1.0);
                RenderTri {
                    i0,
                    i1,
                    i2,
                    normal,
                    diffuse: normal.dot(light_dir).max(0.0),
                    base: palette_quake_stone(height_t as f64),
                }
            })
            .collect::<Vec<_>>();

        (world_vertices, render_tris)
    }

    /// The floor the player would stand on at `(x, y)`, given that their feet
    /// are at `feet_z`.
    ///
    /// The highest floor at or just below the feet wins, so walking under a
    /// balcony keeps you on the ground instead of snapping you to its
    /// underside. If nothing is below - the player is over a pit, or the map
    /// has no floor there - the lowest floor above them is used so they land
    /// somewhere real rather than falling out of the world.
    fn ground_height_at(&self, x: f32, y: f32, feet_z: f32) -> Option<f32> {
        let (below, above) = self.floors_at(x, y, feet_z);
        below.or(above)
    }

    /// The floors under `(x, y)`, split into the highest at or just below the
    /// feet and the lowest above them.
    fn floors_at(&self, x: f32, y: f32, feet_z: f32) -> (Option<f32>, Option<f32>) {
        let mut below: Option<f32> = None;
        let mut above: Option<f32> = None;
        let ceiling = feet_z + QUAKE_STEP_HEIGHT;
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
                if z <= ceiling {
                    if below.is_none_or(|best| z > best) {
                        below = Some(z);
                    }
                } else if above.is_none_or(|best| z < best) {
                    above = Some(z);
                }
            }
        }

        (below, above)
    }

    fn ground_eye_height(&self, x: f32, y: f32, feet_z: f32) -> f32 {
        let ground = self
            .ground_height_at(x, y, feet_z)
            .unwrap_or(self.bounds_min.z);
        ground + QUAKE_EYE_HEIGHT
    }

    /// Whether a body standing at `(x, y)` with its feet at `feet_z` is inside
    /// a wall.
    ///
    /// Only walls the body actually overlaps count. Anything that tops out
    /// below step height is a kerb to walk over, and anything starting above
    /// the player's head is a different storey.
    fn collides(&self, x: f32, y: f32, feet_z: f32) -> bool {
        // Outside the level counts as solid. Otherwise the open-sightline probe
        // would rate "walk off the edge of the map" as the clearest direction,
        // there being no walls out there to stop it.
        if x < self.bounds_min.x + QUAKE_PLAY_MARGIN
            || x > self.bounds_max.x - QUAKE_PLAY_MARGIN
            || y < self.bounds_min.y + QUAKE_PLAY_MARGIN
            || y > self.bounds_max.y - QUAKE_PLAY_MARGIN
        {
            return true;
        }
        let radius_sq = QUAKE_COLLISION_RADIUS * QUAKE_COLLISION_RADIUS;
        let step_z = feet_z + QUAKE_STEP_HEIGHT;
        let head_z = feet_z + QUAKE_BODY_HEIGHT;
        for idx in self.wall_grid.near(x, y, QUAKE_COLLISION_RADIUS) {
            let seg = &self.wall_segments[idx as usize];
            if seg.z_max <= step_z || seg.z_min >= head_z {
                continue;
            }
            let dist_sq = point_segment_distance_sq(x, y, seg.x1, seg.y1, seg.x2, seg.y2);
            if dist_sq < radius_sq {
                return true;
            }
        }
        false
    }

    /// Put the player somewhere they can stand, facing open space.
    ///
    /// The centre of the bounding box is not a place: on this map it is solid
    /// rock well below the lowest floor. Starting there wedged the camera
    /// inside geometry, where every step collided and the level looked frozen.
    pub fn respawn(&mut self) {
        let (spot, yaw) = self.pick_spawn();
        self.player = QuakePlayer::new(spot);
        self.player.yaw = yaw;
        self.move_fwd = 0.0;
        self.move_side = 0.0;
        self.fire_flash = 0.0;
    }

    /// A standing spot and the direction to face, chosen for the longest clear
    /// run ahead.
    ///
    /// Floor area picks the shortlist - a wide triangle is a room, a sliver is
    /// a doorstep - and sightline picks the winner, so the player opens facing
    /// down a hall instead of into a corner.
    fn pick_spawn(&self) -> (Vec3, f32) {
        let mut candidates: Vec<usize> = (0..self.floor_tris.len()).collect();
        candidates.sort_by(|&a, &b| {
            self.floor_tris[b]
                .area
                .abs()
                .total_cmp(&self.floor_tris[a].area.abs())
        });

        let mut best: Option<(f32, Vec3, f32)> = None;
        for &i in candidates.iter().take(QUAKE_SPAWN_CANDIDATES) {
            let tri = &self.floor_tris[i];
            let x = (tri.v0.x + tri.v1.x + tri.v2.x) / 3.0;
            let y = (tri.v0.y + tri.v1.y + tri.v2.y) / 3.0;
            let z = (tri.v0.z + tri.v1.z + tri.v2.z) / 3.0;
            if self.collides(x, y, z) {
                continue;
            }
            let pos = Vec3::new(x, y, z + QUAKE_EYE_HEIGHT);
            let (yaw, reach) = self.most_open_heading(pos);
            if best.is_none_or(|(best_reach, ..)| reach > best_reach) {
                best = Some((reach, pos, yaw));
            }
        }
        if let Some((_, pos, yaw)) = best {
            return (pos, yaw);
        }

        // No clear floor at all: the middle of the map at least keeps the
        // camera inside the level.
        let x = (self.bounds_min.x + self.bounds_max.x) * 0.5;
        let y = (self.bounds_min.y + self.bounds_max.y) * 0.5;
        (
            Vec3::new(x, y, self.ground_eye_height(x, y, self.bounds_min.z)),
            0.0,
        )
    }

    /// The heading with the longest unobstructed run from `from`, and how far
    /// that run goes.
    fn most_open_heading(&self, from: Vec3) -> (f32, f32) {
        let feet = from.z - QUAKE_EYE_HEIGHT;
        let mut best_yaw = 0.0;
        let mut best_reach = -1.0;
        for i in 0..QUAKE_SPAWN_YAW_STEPS {
            let yaw = i as f32 * TAU / QUAKE_SPAWN_YAW_STEPS as f32;
            let (sy, cy) = yaw.sin_cos();
            let mut reach = 0.0;
            for step in 1..=QUAKE_SPAWN_PROBE_STEPS {
                let d = step as f32 * QUAKE_COLLISION_RADIUS;
                if self.collides(from.x + cy * d, from.y + sy * d, feet) {
                    break;
                }
                reach = d;
            }
            if reach > best_reach {
                best_reach = reach;
                best_yaw = yaw;
            }
        }
        (best_yaw, best_reach)
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

        let feet_z = self.player.pos.z - QUAKE_EYE_HEIGHT;
        let travel =
            (self.player.vel.x * self.player.vel.x + self.player.vel.y * self.player.vel.y).sqrt();
        let substep = QUAKE_COLLISION_RADIUS * QUAKE_SUBSTEP_FRACTION;
        let steps = ((travel / substep).ceil() as usize).clamp(1, QUAKE_MAX_SUBSTEPS);
        let mut dx = self.player.vel.x / steps as f32;
        let mut dy = self.player.vel.y / steps as f32;
        for _ in 0..steps {
            if dx != 0.0 {
                let nx = self.player.pos.x + dx;
                if self.collides(nx, self.player.pos.y, feet_z) {
                    self.player.vel.x = 0.0;
                    dx = 0.0;
                } else {
                    self.player.pos.x = nx;
                }
            }
            if dy != 0.0 {
                let ny = self.player.pos.y + dy;
                if self.collides(self.player.pos.x, ny, feet_z) {
                    self.player.vel.y = 0.0;
                    dy = 0.0;
                } else {
                    self.player.pos.y = ny;
                }
            }
        }

        if !self.player.grounded {
            self.player.vel.z += QUAKE_GRAVITY * 0.05;
            self.player.pos.z += self.player.vel.z;
        }

        let ground = self.ground_eye_height(
            self.player.pos.x,
            self.player.pos.y,
            self.player.pos.z - QUAKE_EYE_HEIGHT,
        );

        if self.player.pos.z <= ground {
            self.player.pos.z = ground;
            self.player.vel.z = 0.0;
            self.player.grounded = true;
        } else if self.player.pos.z > ground + QUAKE_STEP_HEIGHT {
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
        let width_usize = width as usize;
        let center = Vec3::new(w * 0.5, h * 0.5, 0.0);
        let eye = self.player.pos;

        let (sy, cy) = self.player.yaw.sin_cos();
        let (sp, cp) = self.player.pitch.sin_cos();
        let forward = Vec3::new(cy * cp, sy * cp, sp).normalized();
        let right = Vec3::new(-sy, cy, 0.0).normalized();
        let up = right.cross(forward).normalized();

        let proj_scale = w.min(h) * 0.9;
        let near = QUAKE_NEAR_PLANE;
        let far = 8.0f32;

        let tri_step = match quality {
            FxQuality::Full => 1,
            FxQuality::Reduced => 2,
            FxQuality::Minimal => 4,
            FxQuality::Off => 0,
        };
        let edge_stride = if tri_step == 1 { 1 } else { tri_step * 2 };

        let edge = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
            (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
        };

        if self.camera_vertices.len() != self.world_vertices.len() {
            self.camera_vertices
                .resize(self.world_vertices.len(), Vec3::new(0.0, 0.0, 0.0));
        }
        for (camera, world) in self.camera_vertices.iter_mut().zip(&self.world_vertices) {
            let rel = *world - eye;
            *camera = Vec3::new(rel.dot(right), rel.dot(up), rel.dot(forward));
        }

        for (tri_idx, tri) in self.render_tris.iter().enumerate().step_by(tri_step) {
            let cam0 = self.camera_vertices[tri.i0];
            let cam1 = self.camera_vertices[tri.i1];
            let cam2 = self.camera_vertices[tri.i2];
            if cam0.z < near && cam1.z < near && cam2.z < near {
                continue;
            }

            let n = tri.normal;
            let w0 = self.world_vertices[tri.i0];
            let view_dir = (eye - w0).normalized();
            let facing = n.dot(view_dir);
            if facing <= 0.02 {
                continue;
            }

            let clipped = clip_triangle_near([cam0, cam1, cam2], near);
            if clipped.len() < 3 {
                continue;
            }

            let diffuse = tri.diffuse;
            let rim = (1.0 - facing.clamp(0.0, 1.0)).powf(3.0) * 0.5;

            let base = tri.base;
            let ambient = 0.15f32;
            let light = (ambient + diffuse * 0.8 + rim).clamp(0.0, 1.5);

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
                let e12_dy = sy2 - sy1;
                let e12_dx = sx2 - sx1;
                let e20_dy = sy0 - sy2;
                let e20_dx = sx0 - sx2;
                let e01_dy = sy1 - sy0;
                let e01_dx = sx1 - sx0;
                let stride_usize = stride;

                for py in (miny..=maxy).step_by(stride_usize) {
                    let fy = py as f32;
                    let row_w0_fy = (fy - sy1) * e12_dx;
                    let row_w1_fy = (fy - sy2) * e20_dx;
                    let row_w2_fy = (fy - sy0) * e01_dx;
                    let mut entered = false;

                    for px in (minx..=maxx).step_by(stride_usize) {
                        let fx = px as f32;
                        let w0e = (fx - sx1) * e12_dy - row_w0_fy;
                        let w1e = (fx - sx2) * e20_dy - row_w1_fy;
                        let w2e = (fx - sx0) * e01_dy - row_w2_fy;

                        if (w0e * area) < 0.0 || (w1e * area) < 0.0 || (w2e * area) < 0.0 {
                            if entered {
                                break;
                            }
                            continue;
                        }
                        entered = true;

                        let b0 = w0e * inv_area;
                        let b1 = w1e * inv_area;
                        let b2 = w2e * inv_area;
                        let z = b0 * a.z + b1 * b.z + b2 * c.z;

                        let idx = py as usize * width_usize + px as usize;
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
                        let lamp = (1.0 - z / QUAKE_LAMP_RANGE).clamp(0.0, 1.0);
                        let mut brightness =
                            (light * fade + lamp * lamp * QUAKE_LAMP_GAIN + grain).clamp(0.0, 1.0);
                        if self.fire_flash > 0.0 {
                            brightness = (brightness + self.fire_flash * 0.4).min(1.3);
                        }

                        let r = (base.r() as f32 * brightness) as u8;
                        let g = (base.g() as f32 * brightness) as u8;
                        let b = (base.b() as f32 * brightness) as u8;
                        painter
                            .point_colored_sparse_at_index_in_bounds(idx, PackedRgba::rgb(r, g, b));
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

            let clipped = clipped.as_slice();
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

/// Dedicated easter-egg screen for the Quake E1M1 renderer.
pub struct QuakeEasterEggScreen {
    quake: RefCell<QuakeE1M1State>,
    painter: RefCell<Painter>,
    quality: FxQuality,
    tick_count: u64,
    time: f64,
}

impl Default for QuakeEasterEggScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl QuakeEasterEggScreen {
    pub fn new() -> Self {
        Self {
            quake: RefCell::new(QuakeE1M1State::default()),
            painter: RefCell::new(Painter::new(0, 0, Mode::Braille)),
            quality: FxQuality::Reduced,
            tick_count: 0,
            time: 0.0,
        }
    }

    fn cycle_quality(&mut self) {
        self.quality = match self.quality {
            FxQuality::Full => FxQuality::Reduced,
            FxQuality::Reduced => FxQuality::Minimal,
            FxQuality::Minimal | FxQuality::Off => FxQuality::Full,
        };
    }

    /// Where the player is standing and which way they face: `(x, y, yaw)`.
    ///
    /// Movement is latched on key-down, so a driver that presses and releases
    /// `w` in the same instant renders a perfectly good frame and never moves.
    /// Only the pose tells those two apart - and it is also how a driver can
    /// tell "walked into a wall and stopped" from "still showing the level".
    pub fn camera_pose(&self) -> (f32, f32, f32) {
        let state = self.quake.borrow();
        (state.player.pos.x, state.player.pos.y, state.player.yaw)
    }

    fn quality_label(&self) -> &'static str {
        match self.quality {
            FxQuality::Full => "Full",
            FxQuality::Reduced => "Reduced",
            FxQuality::Minimal => "Minimal",
            FxQuality::Off => "Off",
        }
    }
}

impl Screen for QuakeEasterEggScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => self.quake.borrow_mut().fire(),
                MouseEventKind::ScrollUp => self.quake.borrow_mut().look(0.0, -0.06),
                MouseEventKind::ScrollDown => self.quake.borrow_mut().look(0.0, 0.06),
                _ => {}
            }
            return Cmd::None;
        }

        if let Event::Key(key) = event {
            match key.kind {
                KeyEventKind::Press | KeyEventKind::Repeat => match key.code {
                    KeyCode::Char('w') | KeyCode::Up => self.quake.borrow_mut().set_move_fwd(1.0),
                    KeyCode::Char('s') | KeyCode::Down => {
                        self.quake.borrow_mut().set_move_fwd(-1.0)
                    }
                    KeyCode::Char('a') => self.quake.borrow_mut().set_move_side(-1.0),
                    KeyCode::Char('d') => self.quake.borrow_mut().set_move_side(1.0),
                    KeyCode::Left => self.quake.borrow_mut().look(-0.11, 0.0),
                    KeyCode::Right => self.quake.borrow_mut().look(0.11, 0.0),
                    KeyCode::Char('j') => self.quake.borrow_mut().look(0.0, 0.07),
                    KeyCode::Char('k') => self.quake.borrow_mut().look(0.0, -0.07),
                    KeyCode::Char(' ') => self.quake.borrow_mut().jump(),
                    KeyCode::Char('f') => self.quake.borrow_mut().fire(),
                    KeyCode::Char('v') => self.cycle_quality(),
                    // Respawn, not rebuild: the level mesh costs 14k triangles
                    // of setup and has not changed.
                    KeyCode::Char('r') => self.quake.borrow_mut().respawn(),
                    _ => {}
                },
                KeyEventKind::Release => match key.code {
                    KeyCode::Char('w') | KeyCode::Char('s') | KeyCode::Up | KeyCode::Down => {
                        self.quake.borrow_mut().set_move_fwd(0.0)
                    }
                    KeyCode::Char('a') | KeyCode::Char('d') => {
                        self.quake.borrow_mut().set_move_side(0.0)
                    }
                    _ => {}
                },
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.time = tick_count as f64 * 0.1;
        self.quake.borrow_mut().update();
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 20 || area.height < 6 {
            Paragraph::new("Need a bit more space for Quake.")
                .style(theme::muted())
                .render(area, frame);
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        let header = format!(
            "WASD move · Arrows look · Space jump · F fire · V quality [{}] · R reset",
            self.quality_label()
        );
        Paragraph::new(header)
            .style(Style::new().fg(theme::fg::SECONDARY).attrs(StyleFlags::DIM))
            .render(rows[0], frame);

        let mut painter = self.painter.borrow_mut();
        painter.ensure_for_area(rows[1], Mode::Braille);
        painter.clear();
        let (pw, ph) = painter.size();
        self.quake.borrow_mut().render(
            &mut painter,
            pw,
            ph,
            self.quality,
            self.time,
            self.tick_count,
        );
        Canvas::from_painter_ref(&painter).render(rows[1], frame);

        let player = self.quake.borrow();
        let status = format!(
            "pos=({:.2},{:.2},{:.2}) yaw={:.2} pitch={:.2} quality={}",
            player.player.pos.x,
            player.player.pos.y,
            player.player.pos.z,
            player.player.yaw,
            player.player.pitch,
            self.quality_label()
        );
        Paragraph::new(status)
            .style(theme::muted())
            .render(rows[2], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "W/A/S/D",
                action: "Move",
            },
            HelpEntry {
                key: "←/→",
                action: "Look yaw",
            },
            HelpEntry {
                key: "j/k",
                action: "Look pitch",
            },
            HelpEntry {
                key: "Space",
                action: "Jump",
            },
            HelpEntry {
                key: "f",
                action: "Fire flash",
            },
            HelpEntry {
                key: "v",
                action: "Cycle quality",
            },
            HelpEntry {
                key: "r",
                action: "Reset run",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Quake E1M1 (Easter Egg)"
    }

    fn tab_label(&self) -> &'static str {
        "Quake"
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

    #[test]
    fn clip_triangle_near_keeps_fully_visible_triangle() {
        let tri = [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(0.0, 1.0, 1.0),
        ];

        let clipped = clip_triangle_near(tri, 0.5);

        assert_eq!(clipped.len(), 3);
        for (actual, expected) in clipped.as_slice().iter().zip(tri) {
            assert!((actual.x - expected.x).abs() < EPS);
            assert!((actual.y - expected.y).abs() < EPS);
            assert!((actual.z - expected.z).abs() < EPS);
        }
    }

    #[test]
    fn clip_triangle_near_expands_one_clipped_vertex_to_quad() {
        let tri = [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(0.5, 1.0, 0.0),
        ];

        let clipped = clip_triangle_near(tri, 0.5);

        assert_eq!(clipped.len(), 4);
        assert!(clipped.as_slice().iter().all(|v| v.z >= 0.5));
    }

    #[test]
    fn clip_triangle_near_discards_fully_hidden_triangle() {
        let tri = [
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(1.0, 0.0, 0.2),
            Vec3::new(0.0, 1.0, 0.3),
        ];

        let clipped = clip_triangle_near(tri, 0.5);

        assert!(clipped.as_slice().is_empty());
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

    // ── clip_triangle_near tests ─────────────────────────────────────

    #[test]
    fn clipped_triangle_starts_empty() {
        let result = ClippedTriangle::new();
        assert!(result.as_slice().is_empty());
    }

    #[test]
    fn clip_all_in_front() {
        let tri = [
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(1.0, 0.0, 2.0),
            Vec3::new(0.0, 1.0, 2.0),
        ];
        let result = clip_triangle_near(tri, 1.0);
        assert_eq!(result.len(), 3, "all-in-front triangle should be unclipped");
    }

    #[test]
    fn clip_all_behind() {
        let tri = [
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(1.0, 0.0, 0.1),
            Vec3::new(0.0, 1.0, 0.1),
        ];
        let result = clip_triangle_near(tri, 1.0);
        assert!(
            result.as_slice().is_empty(),
            "all-behind triangle should be fully clipped"
        );
    }

    #[test]
    fn clip_partial_produces_valid_polygon() {
        // One vertex behind, two in front
        let tri = [
            Vec3::new(0.0, 0.0, 0.5), // Behind near=1.0
            Vec3::new(1.0, 0.0, 2.0), // In front
            Vec3::new(0.0, 1.0, 2.0), // In front
        ];
        let result = clip_triangle_near(tri, 1.0);
        assert!(
            result.len() >= 3,
            "partial clip should produce >= 3 vertices"
        );
        // All clipped vertices must be at or beyond the near plane
        for v in result.as_slice() {
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
    fn floor_tri_valid_computes_bounds() -> Result<(), &'static str> {
        let v0 = Vec3::new(0.0, 0.0, 1.0);
        let v1 = Vec3::new(1.0, 0.0, 1.0);
        let v2 = Vec3::new(0.0, 1.0, 1.0);
        let tri = FloorTri::new(v0, v1, v2).ok_or("triangle should be non-degenerate")?;
        assert!((tri.min_x - 0.0).abs() < EPS);
        assert!((tri.max_x - 1.0).abs() < EPS);
        assert!((tri.min_y - 0.0).abs() < EPS);
        assert!((tri.max_y - 1.0).abs() < EPS);
        assert!(tri.area.abs() > EPS, "area should be nonzero");
        Ok(())
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
    fn level_has_floors_to_stand_on() {
        // Triangles are classified by the *unit* normal. Comparing the raw
        // cross product against 0.35 - as this once did - calls every triangle
        // on a mesh this small a wall, leaving zero floors and a player sealed
        // in rock.
        let state = QuakeE1M1State::default();
        assert!(
            state.floor_tris.len() > 1000,
            "only {} floor triangles: the wall/floor split is broken",
            state.floor_tris.len()
        );
        assert!(!state.wall_segments.is_empty());
    }

    #[test]
    fn spawn_is_a_place_a_player_can_stand() {
        let state = QuakeE1M1State::default();
        let feet = state.player.pos.z - QUAKE_EYE_HEIGHT;
        assert!(
            !state.collides(state.player.pos.x, state.player.pos.y, feet),
            "spawned inside geometry at {:?}",
            (state.player.pos.x, state.player.pos.y, state.player.pos.z)
        );
        assert!(
            state
                .ground_height_at(state.player.pos.x, state.player.pos.y, feet)
                .is_some(),
            "spawned over a void"
        );
        let (_, reach) = state.most_open_heading(state.player.pos);
        assert!(reach > 0.1, "spawn faces a wall {reach} units away");
    }

    #[test]
    fn holding_forward_walks_the_player_across_the_level() {
        // The screen only latches velocity; if collision says every direction
        // is solid the camera never moves and the easter egg looks dead.
        let mut state = QuakeE1M1State::default();
        let start = (state.player.pos.x, state.player.pos.y);
        state.set_move_fwd(1.0);
        for _ in 0..20 {
            state.update();
        }
        let dx = state.player.pos.x - start.0;
        let dy = state.player.pos.y - start.1;
        let travelled = (dx * dx + dy * dy).sqrt();
        assert!(
            travelled > 0.2,
            "two seconds of running covered {travelled:.3} world units"
        );
    }

    #[test]
    fn walls_above_the_head_do_not_block_the_floor_below() {
        // Collision is solved in 2D, so without the height test the walls of an
        // upper room would seal the room beneath it. Find floor that is stood
        // on despite a wall passing directly overhead - a multi-storey map has
        // to have some, and flattened into 2D every bit of it is unreachable.
        let state = QuakeE1M1State::default();
        let radius_sq = QUAKE_COLLISION_RADIUS * QUAKE_COLLISION_RADIUS;
        let mut standable_under_a_wall = 0;

        for tri in &state.floor_tris {
            let x = (tri.v0.x + tri.v1.x + tri.v2.x) / 3.0;
            let y = (tri.v0.y + tri.v1.y + tri.v2.y) / 3.0;
            let feet = (tri.v0.z + tri.v1.z + tri.v2.z) / 3.0;
            let head = feet + QUAKE_BODY_HEIGHT;
            let blocked_if_flattened = state
                .wall_grid
                .near(x, y, QUAKE_COLLISION_RADIUS)
                .map(|i| &state.wall_segments[i as usize])
                .any(|seg| {
                    seg.z_min >= head
                        && point_segment_distance_sq(x, y, seg.x1, seg.y1, seg.x2, seg.y2)
                            < radius_sq
                });
            if blocked_if_flattened && !state.collides(x, y, feet) {
                standable_under_a_wall += 1;
            }
        }

        assert!(
            standable_under_a_wall > 0,
            "no floor on this map is stood on with a wall overhead; either the \
             height test is gone or the level is not what it was"
        );
    }

    #[test]
    fn respawn_is_deterministic() {
        let a = QuakeE1M1State::default();
        let mut b = QuakeE1M1State::default();
        b.set_move_fwd(1.0);
        for _ in 0..10 {
            b.update();
        }
        b.respawn();
        assert_eq!(a.player.pos.x, b.player.pos.x);
        assert_eq!(a.player.pos.y, b.player.pos.y);
        assert_eq!(a.player.yaw, b.player.yaw);
    }

    #[test]
    fn the_substep_cap_covers_top_speed() {
        // Collision is a point test at the end of each substep, so the cap has
        // to leave every substep shorter than the body. If the cap binds before
        // top speed is cut up finely enough, the player walks through walls.
        let target =
            (QUAKE_MOVE_SPEED * QUAKE_MOVE_SPEED + QUAKE_STRAFE_SPEED * QUAKE_STRAFE_SPEED).sqrt();
        // v' = friction * (v + (target - v) * 0.2) settles here.
        let terminal = QUAKE_FRICTION * 0.2 * target / (1.0 - QUAKE_FRICTION * 0.8);
        let substep = QUAKE_COLLISION_RADIUS * QUAKE_SUBSTEP_FRACTION;
        let needed = (terminal / substep).ceil();
        assert!(
            needed <= QUAKE_MAX_SUBSTEPS as f32,
            "top speed {terminal:.4} needs {needed} substeps but the cap is {QUAKE_MAX_SUBSTEPS}"
        );
    }

    #[test]
    fn walking_never_teleports_the_camera_vertically() {
        // A player who stays on the ground should rise and fall by at most a
        // step. `ground_height_at` picking the *highest* floor anywhere under
        // the player - which is what it used to do - snaps the camera onto
        // whatever roof happens to be overhead, mid-stride.
        let mut state = QuakeE1M1State::default();
        // A turning route, so the walk crosses rooms instead of stopping at
        // the first wall it meets.
        state.set_move_fwd(1.0);
        let mut worst = 0.0f32;
        let mut worst_at = (0.0f32, 0.0f32);
        for t in 0..600 {
            if t % 40 == 39 {
                state.look(0.4, 0.0);
            }
            let before_z = state.player.pos.z;
            let grounded_before = state.player.grounded;
            state.update();
            if grounded_before && state.player.grounded {
                let dz = (state.player.pos.z - before_z).abs();
                if dz > worst {
                    worst = dz;
                    worst_at = (state.player.pos.x, state.player.pos.y);
                }
            }
        }
        // Step height plus the slack between a ledge's top edge and the floor
        // triangle that meets it.
        assert!(
            worst <= QUAKE_STEP_HEIGHT * 1.5,
            "camera rose {worst:.4} in one tick near {worst_at:?}, more than a step"
        );
    }

    #[test]
    fn running_does_not_tunnel_through_walls() {
        // A tick of running covers more ground than the body is wide, so the
        // move is substepped. Without that the player teleports through thin
        // geometry; with it, every intermediate position is legal.
        let mut state = QuakeE1M1State::default();
        state.set_move_fwd(1.0);
        for _ in 0..40 {
            state.update();
            let feet = state.player.pos.z - QUAKE_EYE_HEIGHT;
            assert!(
                !state.collides(state.player.pos.x, state.player.pos.y, feet),
                "ended a tick inside a wall at {:?}",
                (state.player.pos.x, state.player.pos.y)
            );
        }
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
        let margin = QUAKE_PLAY_MARGIN;
        state.set_move_fwd(1.0);
        for _ in 0..500 {
            state.update();
        }
        assert!(state.player.pos.x >= state.bounds_min.x + margin - EPS);
        assert!(state.player.pos.x <= state.bounds_max.x - margin + EPS);
        assert!(state.player.pos.y >= state.bounds_min.y + margin - EPS);
        assert!(state.player.pos.y <= state.bounds_max.y - margin + EPS);
    }

    fn painted_pixels(painter: &Painter) -> usize {
        let (width, height) = painter.size();
        let mut count = 0;
        for y in 0..i32::from(height) {
            for x in 0..i32::from(width) {
                if painter.get(x, y) {
                    count += 1;
                }
            }
        }
        count
    }

    fn render_pixel_count(quality: FxQuality) -> usize {
        let mut state = QuakeE1M1State::default();
        let mut painter = Painter::new(96, 48, Mode::Braille);
        state.render(&mut painter, 96, 48, quality, 0.0, 0);
        painted_pixels(&painter)
    }

    #[test]
    fn quality_tiers_reduce_rendered_triangle_work() {
        let full = render_pixel_count(FxQuality::Full);
        let reduced = render_pixel_count(FxQuality::Reduced);
        let minimal = render_pixel_count(FxQuality::Minimal);

        assert!(full > 0, "full quality should render visible geometry");
        assert!(
            reduced > 0,
            "reduced quality should render visible geometry"
        );
        assert!(
            minimal > 0,
            "minimal quality should render visible geometry"
        );
        assert!(
            reduced < full,
            "reduced quality should paint fewer pixels than full quality: {reduced} >= {full}"
        );
        assert!(
            minimal <= reduced,
            "minimal quality should not paint more pixels than reduced quality: {minimal} > {reduced}"
        );
    }

    fn centre_coverage(painter: &Painter) -> (usize, usize) {
        let (width, height) = painter.size();
        let (w, h) = (i32::from(width), i32::from(height));
        let (x0, x1) = (w * 2 / 5, w * 3 / 5);
        let (y0, y1) = (h * 2 / 5, h * 3 / 5);
        let mut painted = 0;
        let mut total = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                total += 1;
                if painter.get(x, y) {
                    painted += 1;
                }
            }
        }
        (painted, total)
    }

    #[test]
    fn a_wall_at_arms_length_is_lit_by_the_player() {
        // Pressed against a wall, the whole view is that wall - one collision
        // radius away. The scene's single fixed light leaves such a face at
        // ambient, which is very nearly black, so the camera carries its own
        // light. Without it the player walks up to a wall and the screen goes
        // dark, which reads exactly like a crash.
        let mut state = QuakeE1M1State::default();
        state.player.pos = Vec3::new(-0.42, 0.08, -0.91);
        state.player.yaw = std::f32::consts::PI;
        let mut painter = Painter::new(160, 80, Mode::Braille);
        state.render(&mut painter, 160, 80, FxQuality::Full, 0.0, 0);

        let (pw, ph) = painter.size();
        let (w, h) = (usize::from(pw), usize::from(ph));
        let mut nearest = f32::INFINITY;
        for y in (h * 2 / 5)..(h * 3 / 5) {
            for x in (w * 2 / 5)..(w * 3 / 5) {
                let z = state.depth[y * w + x];
                if z.is_finite() {
                    nearest = nearest.min(z);
                }
            }
        }
        assert!(
            nearest.is_finite() && nearest < QUAKE_LAMP_RANGE,
            "expected the wall to fill the view within lamp range, nearest surface {nearest}"
        );
        let lamp = (1.0 - nearest / QUAKE_LAMP_RANGE).clamp(0.0, 1.0);
        assert!(
            lamp * lamp * QUAKE_LAMP_GAIN > 0.25,
            "the player's light adds only {} at {nearest} away",
            lamp * lamp * QUAKE_LAMP_GAIN
        );
    }

    #[test]
    fn walls_the_player_walks_into_are_still_drawn() {
        // Walk into a wall from many spots and check the middle of the view is
        // a wall rather than a hole. When the near plane sat outside the body
        // radius, this averaged 0.39 of the centre painted, with two thirds of
        // the views more than half empty: the player pressed their face to a
        // wall and saw straight through the level.
        let template = QuakeE1M1State::default();
        let mut worst = (usize::MAX, 0usize, (0.0f32, 0.0f32), 0.0f32);
        let mut sampled = 0;
        let mut coverage: Vec<f32> = Vec::new();
        for (i, tri) in template.floor_tris.iter().enumerate() {
            if i % 17 != 0 {
                continue;
            }
            let x = (tri.v0.x + tri.v1.x + tri.v2.x) / 3.0;
            let y = (tri.v0.y + tri.v1.y + tri.v2.y) / 3.0;
            let z = (tri.v0.z + tri.v1.z + tri.v2.z) / 3.0;
            if template.collides(x, y, z) {
                continue;
            }
            for step in 0..8 {
                let yaw = step as f32 * TAU / 8.0;
                let mut state = template.clone();
                state.player.pos = Vec3::new(x, y, z + QUAKE_EYE_HEIGHT);
                state.player.yaw = yaw;
                state.set_move_fwd(1.0);
                for _ in 0..25 {
                    state.update();
                }
                let mut painter = Painter::new(96, 48, Mode::Braille);
                state.render(&mut painter, 96, 48, FxQuality::Full, 0.0, 0);
                let (painted, total) = centre_coverage(&painter);
                sampled += 1;
                coverage.push(painted as f32 / total as f32);
                if painted < worst.0 {
                    worst = (
                        painted,
                        total,
                        (state.player.pos.x, state.player.pos.y),
                        yaw,
                    );
                }
            }
        }
        let mean = coverage.iter().sum::<f32>() / coverage.len() as f32;
        let mostly_empty = coverage.iter().filter(|c| **c < 0.5).count();
        assert!(sampled > 200, "only sampled {sampled} views");
        assert!(
            mean > 0.7,
            "the middle of the view averages {mean:.3} painted across {sampled} walls; \
             worst {}/{} at {:?} facing {:.2}",
            worst.0,
            worst.1,
            worst.2,
            worst.3
        );
        assert!(
            mostly_empty * 4 < sampled,
            "{mostly_empty} of {sampled} wall-facing views are more than half empty"
        );
    }

    #[test]
    fn quality_off_renders_nothing() {
        assert_eq!(render_pixel_count(FxQuality::Off), 0);
    }
}
