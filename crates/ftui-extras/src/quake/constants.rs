//! Game constants ported from the Quake 1 engine (id Software GPL).

/// BSP file version (Quake 1).
pub const BSPVERSION: i32 = 29;

/// Renderer framebuffer resolution.
pub const SCREENWIDTH: u32 = 320;
pub const SCREENHEIGHT: u32 = 200;

/// Field of view.
pub const FOV_DEGREES: f32 = 90.0;

/// Near clip plane distance.
pub const NEAR_CLIP: f32 = 4.0;

/// Player constants (from Quake source: sv_move.c, sv_phys.c).
pub const PLAYER_HEIGHT: f32 = 56.0;
pub const PLAYER_VIEW_HEIGHT: f32 = 22.0; // eye_position in Quake (22 units above origin)
pub const PLAYER_RADIUS: f32 = 16.0;
pub const STEPSIZE: f32 = 18.0; // max step-up height
pub const PLAYER_MOVE_SPEED: f32 = 320.0; // units per second
pub const PLAYER_STRAFE_SPEED: f32 = 320.0;
pub const PLAYER_RUN_MULT: f32 = 2.0;
pub const PLAYER_JUMP_VELOCITY: f32 = 270.0; // upward velocity on jump

/// Physics constants (from Quake sv_phys.c).
pub const SV_GRAVITY: f32 = 800.0; // gravity acceleration (units/sec^2)
pub const SV_FRICTION: f32 = 4.0;
pub const SV_STOPSPEED: f32 = 100.0;
pub const SV_MAXVELOCITY: f32 = 2000.0;

/// Game tick rate (Quake runs at 72 Hz server tick).
pub const TICKRATE: u32 = 72;
pub const TICK_SECS: f64 = 1.0 / TICKRATE as f64;

/// BSP contents types.
pub const CONTENTS_EMPTY: i32 = -1;
pub const CONTENTS_SOLID: i32 = -2;
pub const CONTENTS_WATER: i32 = -3;
pub const CONTENTS_SLIME: i32 = -4;
pub const CONTENTS_LAVA: i32 = -5;
pub const CONTENTS_SKY: i32 = -6;

/// BSP lump indices.
pub const LUMP_ENTITIES: usize = 0;
pub const LUMP_PLANES: usize = 1;
pub const LUMP_TEXTURES: usize = 2;
pub const LUMP_VERTEXES: usize = 3;
pub const LUMP_VISIBILITY: usize = 4;
pub const LUMP_NODES: usize = 5;
pub const LUMP_TEXINFO: usize = 6;
pub const LUMP_FACES: usize = 7;
pub const LUMP_LIGHTING: usize = 8;
pub const LUMP_CLIPNODES: usize = 9;
pub const LUMP_LEAFS: usize = 10;
pub const LUMP_MARKSURFACES: usize = 11;
pub const LUMP_EDGES: usize = 12;
pub const LUMP_SURFEDGES: usize = 13;
pub const LUMP_MODELS: usize = 14;
pub const HEADER_LUMPS: usize = 15;

/// Maximum number of light styles per face.
pub const MAXLIGHTMAPS: usize = 4;

/// Procedural map colors.
pub const WALL_COLORS: [[u8; 3]; 8] = [
    [120, 100, 80],  // Brown stone
    [100, 100, 110], // Blue-gray metal
    [90, 80, 70],    // Dark brown
    [80, 90, 100],   // Steel blue
    [110, 90, 75],   // Tan
    [70, 70, 80],    // Dark gray
    [130, 110, 90],  // Light brown
    [85, 85, 95],    // Slate
];

pub const SKY_TOP: [u8; 3] = [60, 40, 30];
pub const SKY_BOTTOM: [u8; 3] = [100, 80, 60];

pub const FLOOR_NEAR: [u8; 3] = [80, 70, 60];
pub const FLOOR_FAR: [u8; 3] = [50, 45, 40];

pub const CEILING_COLOR: [u8; 3] = [60, 55, 50];

/// Fog distance constants (Quake-style brown/dark atmosphere).
pub const FOG_START: f32 = 50.0;
pub const FOG_END: f32 = 800.0;
pub const FOG_COLOR: [u8; 3] = [40, 35, 30];
