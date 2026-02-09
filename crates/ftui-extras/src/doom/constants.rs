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
/// Full body height for passage checking (56 map units in original Doom).
pub const PLAYER_HEIGHT: f32 = 56.0;
/// Eye level above floor (41 map units in original Doom).
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

#[cfg(test)]
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    // ── Angle system ────────────────────────────────────────────────

    #[test]
    fn fine_angles_is_power_of_two() {
        assert!(FINEANGLES.is_power_of_two());
        assert_eq!(FINEANGLES, 8192);
    }

    #[test]
    fn finemask_masks_correctly() {
        assert_eq!(FINEMASK, FINEANGLES - 1);
        // Mask should wrap values back into range
        assert_eq!(FINEANGLES & FINEMASK, 0);
        assert_eq!((FINEANGLES + 1) & FINEMASK, 1);
        assert_eq!((FINEANGLES - 1) & FINEMASK, FINEANGLES - 1);
    }

    #[test]
    fn bam_angles_quarter_circle() {
        // BAM (Binary Angle Measurement) uses full u32 range for 360 degrees
        assert_eq!(ANG90, 0x4000_0000);
        assert_eq!(ANG180, 0x8000_0000);
        assert_eq!(ANG270, 0xC000_0000);
        // Quadrant relationships
        assert_eq!(ANG180, ANG90.wrapping_mul(2));
        assert_eq!(ANG270, ANG90.wrapping_mul(3));
        // Full circle wraps to 0
        assert_eq!(ANG90.wrapping_mul(4), ANG_COUNT);
    }

    #[test]
    fn ang_count_wraps_to_zero() {
        // ANG_COUNT is 2^32 which wraps to 0 in u32
        assert_eq!(ANG_COUNT, 0);
    }

    // ── Fixed-point system ──────────────────────────────────────────

    #[test]
    fn fixed_point_16_16() {
        assert_eq!(FRACBITS, 16);
        assert_eq!(FRACUNIT, 1 << 16);
        assert_eq!(FRACUNIT, 65536);
    }

    #[test]
    fn fracunit_represents_one() {
        // FRACUNIT is 1.0 in fixed-point: shifting right by FRACBITS recovers integer
        assert_eq!(FRACUNIT >> FRACBITS, 1);
        assert_eq!((FRACUNIT * 3) >> FRACBITS, 3);
    }

    // ── Player constants ────────────────────────────────────────────

    #[test]
    fn player_view_below_full_height() {
        // Eye level must be below full body height
        assert!(PLAYER_VIEW_HEIGHT < PLAYER_HEIGHT);
    }

    #[test]
    fn player_step_below_view_height() {
        // Step height must be below eye level (can't step over your own eyes)
        assert!(PLAYER_STEP_HEIGHT < PLAYER_VIEW_HEIGHT);
    }

    #[test]
    fn player_speeds_positive() {
        assert!(PLAYER_MOVE_SPEED > 0.0);
        assert!(PLAYER_STRAFE_SPEED > 0.0);
        assert!(PLAYER_TURN_SPEED > 0.0);
        assert!(PLAYER_MAX_MOVE > 0.0);
    }

    #[test]
    fn player_run_multiplier_greater_than_one() {
        assert!(PLAYER_RUN_MULT > 1.0);
    }

    #[test]
    fn player_friction_in_unit_range() {
        // Friction should be in (0, 1) for deceleration
        assert!(PLAYER_FRICTION > 0.0);
        assert!(PLAYER_FRICTION < 1.0);
    }

    #[test]
    fn player_radius_positive() {
        assert!(PLAYER_RADIUS > 0.0);
    }

    // ── Physics ─────────────────────────────────────────────────────

    #[test]
    fn gravity_positive() {
        assert!(GRAVITY > 0.0);
    }

    #[test]
    fn tick_rate_matches_original_doom() {
        assert_eq!(TICRATE, 35);
    }

    #[test]
    fn doom_tick_duration_reciprocal() {
        let expected = 1.0 / 35.0;
        assert!((DOOM_TICK_SECS - expected).abs() < 1e-10);
    }

    // ── Renderer constants ──────────────────────────────────────────

    #[test]
    fn screen_dimensions_positive() {
        assert!(SCREENWIDTH > 0);
        assert!(SCREENHEIGHT > 0);
    }

    #[test]
    fn fov_is_90_degrees() {
        assert_eq!(FOV_DEGREES, 90.0);
        assert!((FOV_RADIANS - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn maxopenings_proportional_to_screen() {
        // MAXOPENINGS = SCREENWIDTH * 64
        assert_eq!(MAXOPENINGS, SCREENWIDTH as usize * 64);
    }

    #[test]
    fn renderer_limits_positive() {
        assert!(MAXDRAWSEGS > 0);
        assert!(MAXVISPLANES > 0);
        assert!(MAXOPENINGS > 0);
    }

    // ── Texture / lighting ──────────────────────────────────────────

    #[test]
    fn wall_texture_height_positive() {
        assert!(WALL_TEX_HEIGHT > 0.0);
    }

    #[test]
    fn light_range_full_byte() {
        assert_eq!(LIGHT_MIN, 0);
        assert_eq!(LIGHT_MAX, 255);
    }

    #[test]
    fn colormap_levels_positive() {
        assert!(COLORMAP_LEVELS > 0);
    }

    #[test]
    fn sky_flat_name_not_empty() {
        assert!(!SKY_FLAT_NAME.is_empty());
    }
}
