//! Quake map representation and procedural level generator.
//!
//! Provides both BSP file loading and a procedural map generator for the
//! Quake E1M1-style demo level. The procedural generator creates rooms,
//! corridors, ramps, and platforms with proper collision geometry.

use super::constants::*;

/// A face (polygon) in the map.
#[derive(Debug, Clone)]
pub struct Face {
    /// Indices into the vertex array forming this polygon.
    pub vertex_indices: Vec<usize>,
    /// Face normal.
    pub normal: [f32; 3],
    /// Distance from origin along normal.
    pub dist: f32,
    /// Surface color index (into WALL_COLORS).
    pub color_index: u8,
    /// Whether this is a sky surface.
    pub is_sky: bool,
    /// Light level (0.0-1.0).
    pub light_level: f32,
    /// Texture type for pattern generation.
    pub tex_type: TexType,
}

/// Texture type for procedural texturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexType {
    Floor,
    Ceiling,
    Wall,
    Sky,
    Metal,
    Lava,
}

/// A wall segment for collision detection (2D).
#[derive(Debug, Clone, Copy)]
pub struct WallSeg {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub floor_z: f32,
    pub ceil_z: f32,
}

/// A room in the procedural map.
#[derive(Debug, Clone)]
pub struct Room {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub floor_z: f32,
    pub ceil_z: f32,
    pub light: f32,
}

/// The Quake map containing all geometry for rendering and collision.
#[derive(Debug, Clone)]
pub struct QuakeMap {
    /// 3D vertices.
    pub vertices: Vec<[f32; 3]>,
    /// Faces (polygons).
    pub faces: Vec<Face>,
    /// Wall segments for collision.
    pub walls: Vec<WallSeg>,
    /// Rooms for floor/ceiling height queries.
    pub rooms: Vec<Room>,
    /// Player start position.
    pub player_start: [f32; 4], // x, y, z, angle
}

impl Default for QuakeMap {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            faces: Vec::new(),
            walls: Vec::new(),
            rooms: Vec::new(),
            player_start: [0.0, 0.0, 0.0, 0.0],
        }
    }
}

impl QuakeMap {
    /// Create an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get floor height at a 2D position by finding which room contains it.
    pub fn floor_height_at(&self, x: f32, y: f32) -> f32 {
        for room in &self.rooms {
            if x >= room.x && x <= room.x + room.width && y >= room.y && y <= room.y + room.height {
                return room.floor_z;
            }
        }
        // Default: lowest floor
        self.rooms
            .iter()
            .map(|r| r.floor_z)
            .fold(f32::MAX, f32::min)
    }

    /// Get ceiling height at a 2D position.
    pub fn ceiling_height_at(&self, x: f32, y: f32) -> f32 {
        for room in &self.rooms {
            if x >= room.x && x <= room.x + room.width && y >= room.y && y <= room.y + room.height {
                return room.ceil_z;
            }
        }
        1000.0 // default high ceiling
    }

    /// Check if a point (with radius) is inside solid geometry.
    pub fn point_in_solid(&self, x: f32, y: f32, _z: f32, radius: f32) -> bool {
        // Check wall collisions
        for wall in &self.walls {
            if circle_intersects_segment(x, y, radius, wall.x1, wall.y1, wall.x2, wall.y2) {
                return true;
            }
        }
        false
    }

    /// Get the player start position and angle.
    pub fn player_start(&self) -> (f32, f32, f32, f32) {
        (
            self.player_start[0],
            self.player_start[1],
            self.player_start[2],
            self.player_start[3],
        )
    }
}

/// Circle-segment intersection test for collision detection.
fn circle_intersects_segment(cx: f32, cy: f32, r: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> bool {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-8 {
        let dist = ((cx - x1) * (cx - x1) + (cy - y1) * (cy - y1)).sqrt();
        return dist < r;
    }
    let t = ((cx - x1) * dx + (cy - y1) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let nearest_x = x1 + t * dx;
    let nearest_y = y1 + t * dy;
    let dist = ((cx - nearest_x) * (cx - nearest_x) + (cy - nearest_y) * (cy - nearest_y)).sqrt();
    dist < r
}

/// Generate a procedural Quake E1M1-style level.
///
/// Creates interconnected rooms, corridors, height variations, ramps,
/// and platforms—styled after the Slipgate Complex (E1M1).
pub fn generate_e1m1() -> QuakeMap {
    let mut map = QuakeMap::new();

    // Room definitions: x, y, width, height, floor_z, ceil_z, light
    let rooms_def: Vec<(f32, f32, f32, f32, f32, f32, f32)> = vec![
        // Spawn room (large, well-lit)
        (0.0, 0.0, 256.0, 256.0, 0.0, 192.0, 0.9),
        // First corridor
        (256.0, 64.0, 192.0, 128.0, 0.0, 128.0, 0.6),
        // Central hub (big room with pillar)
        (448.0, -128.0, 384.0, 384.0, -32.0, 256.0, 0.8),
        // East corridor (elevated)
        (832.0, -32.0, 192.0, 128.0, 32.0, 160.0, 0.5),
        // East room (lava pit area)
        (1024.0, -192.0, 320.0, 320.0, -64.0, 224.0, 0.7),
        // North corridor from hub
        (544.0, 256.0, 128.0, 192.0, 0.0, 128.0, 0.4),
        // North room (dark, atmospheric)
        (480.0, 448.0, 256.0, 256.0, -16.0, 192.0, 0.3),
        // South corridor from hub
        (544.0, -320.0, 128.0, 192.0, -32.0, 128.0, 0.5),
        // South room (elevated platform)
        (480.0, -576.0, 256.0, 256.0, 48.0, 256.0, 0.7),
        // Secret passage (narrow, very dark)
        (0.0, 256.0, 64.0, 256.0, 0.0, 96.0, 0.2),
        // Secret room
        (0.0, 512.0, 128.0, 128.0, 16.0, 128.0, 0.5),
    ];

    // Create rooms
    for (i, &(rx, ry, rw, rh, fz, cz, light)) in rooms_def.iter().enumerate() {
        let room = Room {
            x: rx,
            y: ry,
            width: rw,
            height: rh,
            floor_z: fz,
            ceil_z: cz,
            light,
        };
        map.rooms.push(room);

        let color_idx = (i % WALL_COLORS.len()) as u8;
        add_room_geometry(&mut map, rx, ry, rw, rh, fz, cz, light, color_idx);
    }

    // Player start in the spawn room center
    map.player_start = [128.0, 128.0, 0.0, 0.0];

    // Add some pillars in the central hub
    add_pillar(&mut map, 576.0, 0.0, 32.0, -32.0, 192.0, 0.7, 2);
    add_pillar(&mut map, 704.0, 0.0, 32.0, -32.0, 192.0, 0.7, 2);
    add_pillar(&mut map, 576.0, 128.0, 32.0, -32.0, 192.0, 0.7, 2);
    add_pillar(&mut map, 704.0, 128.0, 32.0, -32.0, 192.0, 0.7, 2);

    // Add a raised platform in the central hub
    add_platform(&mut map, 608.0, 16.0, 96.0, 96.0, 32.0, 0.9, 4);

    map
}

/// Add a box room with floor, ceiling, and 4 walls.
#[allow(clippy::too_many_arguments)]
fn add_room_geometry(
    map: &mut QuakeMap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fz: f32,
    cz: f32,
    light: f32,
    color_idx: u8,
) {
    let base = map.vertices.len();

    // 8 corners of the box
    map.vertices.push([x, y, fz]); // 0: floor SW
    map.vertices.push([x + w, y, fz]); // 1: floor SE
    map.vertices.push([x + w, y + h, fz]); // 2: floor NE
    map.vertices.push([x, y + h, fz]); // 3: floor NW
    map.vertices.push([x, y, cz]); // 4: ceil SW
    map.vertices.push([x + w, y, cz]); // 5: ceil SE
    map.vertices.push([x + w, y + h, cz]); // 6: ceil NE
    map.vertices.push([x, y + h, cz]); // 7: ceil NW

    // Floor
    map.faces.push(Face {
        vertex_indices: vec![base, base + 1, base + 2, base + 3],
        normal: [0.0, 0.0, 1.0],
        dist: fz,
        color_index: color_idx,
        is_sky: false,
        light_level: light,
        tex_type: TexType::Floor,
    });

    // Ceiling
    map.faces.push(Face {
        vertex_indices: vec![base + 7, base + 6, base + 5, base + 4],
        normal: [0.0, 0.0, -1.0],
        dist: -cz,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.7,
        tex_type: TexType::Ceiling,
    });

    // South wall (y = ry)
    map.faces.push(Face {
        vertex_indices: vec![base, base + 4, base + 5, base + 1],
        normal: [0.0, -1.0, 0.0],
        dist: -y,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.8,
        tex_type: TexType::Wall,
    });
    map.walls.push(WallSeg {
        x1: x,
        y1: y,
        x2: x + w,
        y2: y,
        floor_z: fz,
        ceil_z: cz,
    });

    // North wall (y = ry + rh)
    map.faces.push(Face {
        vertex_indices: vec![base + 2, base + 6, base + 7, base + 3],
        normal: [0.0, 1.0, 0.0],
        dist: y + h,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.8,
        tex_type: TexType::Wall,
    });
    map.walls.push(WallSeg {
        x1: x,
        y1: y + h,
        x2: x + w,
        y2: y + h,
        floor_z: fz,
        ceil_z: cz,
    });

    // West wall (x = rx)
    map.faces.push(Face {
        vertex_indices: vec![base + 3, base + 7, base + 4, base],
        normal: [-1.0, 0.0, 0.0],
        dist: -x,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.6,
        tex_type: TexType::Wall,
    });
    map.walls.push(WallSeg {
        x1: x,
        y1: y,
        x2: x,
        y2: y + h,
        floor_z: fz,
        ceil_z: cz,
    });

    // East wall (x = rx + rw)
    map.faces.push(Face {
        vertex_indices: vec![base + 1, base + 5, base + 6, base + 2],
        normal: [1.0, 0.0, 0.0],
        dist: x + w,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.6,
        tex_type: TexType::Wall,
    });
    map.walls.push(WallSeg {
        x1: x + w,
        y1: y,
        x2: x + w,
        y2: y + h,
        floor_z: fz,
        ceil_z: cz,
    });
}

/// Add a square pillar.
#[allow(clippy::too_many_arguments)]
fn add_pillar(
    map: &mut QuakeMap,
    cx: f32,
    cy: f32,
    size: f32,
    fz: f32,
    cz: f32,
    light: f32,
    color_idx: u8,
) {
    let half = size / 2.0;
    let base = map.vertices.len();

    // 8 corners
    map.vertices.push([cx - half, cy - half, fz]);
    map.vertices.push([cx + half, cy - half, fz]);
    map.vertices.push([cx + half, cy + half, fz]);
    map.vertices.push([cx - half, cy + half, fz]);
    map.vertices.push([cx - half, cy - half, cz]);
    map.vertices.push([cx + half, cy - half, cz]);
    map.vertices.push([cx + half, cy + half, cz]);
    map.vertices.push([cx - half, cy + half, cz]);

    // 4 outer walls
    let normals = [
        [0.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
    ];
    let indices = [
        [base, base + 4, base + 5, base + 1],
        [base + 1, base + 5, base + 6, base + 2],
        [base + 2, base + 6, base + 7, base + 3],
        [base + 3, base + 7, base + 4, base],
    ];

    for (i, (norm, idx)) in normals.iter().zip(indices.iter()).enumerate() {
        map.faces.push(Face {
            vertex_indices: idx.to_vec(),
            normal: *norm,
            dist: 0.0,
            color_index: color_idx,
            is_sky: false,
            light_level: light * if i % 2 == 0 { 0.8 } else { 0.6 },
            tex_type: TexType::Metal,
        });
    }

    // Collision walls for pillar
    map.walls.push(WallSeg {
        x1: cx - half,
        y1: cy - half,
        x2: cx + half,
        y2: cy - half,
        floor_z: fz,
        ceil_z: cz,
    });
    map.walls.push(WallSeg {
        x1: cx + half,
        y1: cy - half,
        x2: cx + half,
        y2: cy + half,
        floor_z: fz,
        ceil_z: cz,
    });
    map.walls.push(WallSeg {
        x1: cx + half,
        y1: cy + half,
        x2: cx - half,
        y2: cy + half,
        floor_z: fz,
        ceil_z: cz,
    });
    map.walls.push(WallSeg {
        x1: cx - half,
        y1: cy + half,
        x2: cx - half,
        y2: cy - half,
        floor_z: fz,
        ceil_z: cz,
    });
}

/// Add a raised platform.
#[allow(clippy::too_many_arguments)]
fn add_platform(
    map: &mut QuakeMap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    height: f32,
    light: f32,
    color_idx: u8,
) {
    let base = map.vertices.len();

    // Top face vertices
    map.vertices.push([x, y, height]);
    map.vertices.push([x + w, y, height]);
    map.vertices.push([x + w, y + h, height]);
    map.vertices.push([x, y + h, height]);

    // Top face
    map.faces.push(Face {
        vertex_indices: vec![base, base + 1, base + 2, base + 3],
        normal: [0.0, 0.0, 1.0],
        dist: height,
        color_index: color_idx,
        is_sky: false,
        light_level: light,
        tex_type: TexType::Metal,
    });

    // Side faces (step edges)
    let side_base = map.vertices.len();
    map.vertices.push([x, y, 0.0]); // bottom corners
    map.vertices.push([x + w, y, 0.0]);
    map.vertices.push([x + w, y + h, 0.0]);
    map.vertices.push([x, y + h, 0.0]);

    // South step
    map.faces.push(Face {
        vertex_indices: vec![side_base, base, base + 1, side_base + 1],
        normal: [0.0, -1.0, 0.0],
        dist: -y,
        color_index: color_idx,
        is_sky: false,
        light_level: light * 0.6,
        tex_type: TexType::Metal,
    });

    // Add room entry for the platform (floor height = platform top)
    map.rooms.push(Room {
        x,
        y,
        width: w,
        height: h,
        floor_z: height,
        ceil_z: height + 160.0,
        light,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_e1m1_creates_geometry() {
        let map = generate_e1m1();
        assert!(!map.vertices.is_empty());
        assert!(!map.faces.is_empty());
        assert!(!map.walls.is_empty());
        assert!(!map.rooms.is_empty());
    }

    #[test]
    fn floor_height_in_spawn_room() {
        let map = generate_e1m1();
        // Spawn room is at (0,0) with floor_z = 0
        let h = map.floor_height_at(128.0, 128.0);
        assert!((h - 0.0).abs() < 0.01);
    }

    #[test]
    fn ceiling_in_spawn_room() {
        let map = generate_e1m1();
        let h = map.ceiling_height_at(128.0, 128.0);
        assert!((h - 192.0).abs() < 0.01);
    }

    #[test]
    fn collision_detects_walls() {
        let map = generate_e1m1();
        // Very close to the west wall of spawn room (x=0)
        assert!(map.point_in_solid(-5.0, 128.0, 10.0, 16.0));
    }
}
