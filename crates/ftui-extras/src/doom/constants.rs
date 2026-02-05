//! Game constants matching the original Doom engine.

/// Number of fine angles (2^13 = 8192) used for trig tables.
pub const FINEANGLES: usize = 8192;
/// Mask for fine angle wrapping.
pub const FINEMASK: usize = FINEANGLES - 1;
/// Number of angles in a full circle (Doom BAM system).
pub const ANG_COUNT: u32 = 0x1_0000_0000_u64 as u32; // wraps to 0
/// 90 degrees in BAM.
pub const ANG90: u32 = 0x4000_0000;
/// 180 degrees in BAM.
pub const ANG180: u32 = 0x8000_0000;
/// 270 degrees in BAM.
pub const ANG270: u32 = 0xC000_0000;

/// Doom fixed-point: 16.16
pub const FRACBITS: i32 = 16;
pub const FRACUNIT: i32 = 1 << FRACBITS;

/// Player constants.
pub const PLAYER_HEIGHT: f32 = 41.0;
pub const PLAYER_VIEW_HEIGHT: f32 = 41.0;
pub const PLAYER_RADIUS: f32 = 16.0;
pub const PLAYER_MAX_MOVE: f32 = 30.0;
pub const PLAYER_MOVE_SPEED: f32 = 3.0;
pub const PLAYER_STRAFE_SPEED: f32 = 2.5;
pub const PLAYER_TURN_SPEED: f32 = 0.06;
pub const PLAYER_RUN_MULT: f32 = 2.0;
pub const PLAYER_FRICTION: f32 = 0.90625; // 0xe800 / 0x10000
pub const PLAYER_STEP_HEIGHT: f32 = 24.0;

/// Gravity in map units per tic squared.
pub const GRAVITY: f32 = 1.0;

/// Game tick rate (35 Hz like original Doom).
pub const TICRATE: u32 = 35;
pub const DOOM_TICK_SECS: f64 = 1.0 / TICRATE as f64;

/// Renderer constants.
pub const SCREENWIDTH: u32 = 320;
pub const SCREENHEIGHT: u32 = 200;
pub const FOV_DEGREES: f32 = 90.0;
pub const FOV_RADIANS: f32 = std::f32::consts::FRAC_PI_2;

/// Maximum number of drawsegs.
pub const MAXDRAWSEGS: usize = 256;
/// Maximum number of visplanes.
pub const MAXVISPLANES: usize = 128;
/// Maximum number of openings (clip ranges).
pub const MAXOPENINGS: usize = 320 * 64;

/// Wall texture height in map units.
pub const WALL_TEX_HEIGHT: f32 = 128.0;

/// Sky flat name.
pub const SKY_FLAT_NAME: &str = "F_SKY1";

/// Minimum light level.
pub const LIGHT_MIN: u8 = 0;
/// Maximum light level.
pub const LIGHT_MAX: u8 = 255;
/// Number of light levels in COLORMAP.
pub const COLORMAP_LEVELS: usize = 34;
