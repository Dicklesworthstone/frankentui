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

#[cfg(test)]
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    // ── BSP format ──────────────────────────────────────────────────

    #[test]
    fn bsp_version_quake1() {
        assert_eq!(BSPVERSION, 29);
    }

    #[test]
    fn bsp_lump_indices_contiguous() {
        // Lumps must be sequential 0..HEADER_LUMPS
        assert_eq!(LUMP_ENTITIES, 0);
        assert_eq!(LUMP_PLANES, 1);
        assert_eq!(LUMP_TEXTURES, 2);
        assert_eq!(LUMP_VERTEXES, 3);
        assert_eq!(LUMP_VISIBILITY, 4);
        assert_eq!(LUMP_NODES, 5);
        assert_eq!(LUMP_TEXINFO, 6);
        assert_eq!(LUMP_FACES, 7);
        assert_eq!(LUMP_LIGHTING, 8);
        assert_eq!(LUMP_CLIPNODES, 9);
        assert_eq!(LUMP_LEAFS, 10);
        assert_eq!(LUMP_MARKSURFACES, 11);
        assert_eq!(LUMP_EDGES, 12);
        assert_eq!(LUMP_SURFEDGES, 13);
        assert_eq!(LUMP_MODELS, 14);
    }

    #[test]
    fn header_lumps_is_count_of_all_lumps() {
        assert_eq!(HEADER_LUMPS, LUMP_MODELS + 1);
        assert_eq!(HEADER_LUMPS, 15);
    }

    // ── BSP contents ────────────────────────────────────────────────

    #[test]
    fn bsp_contents_are_negative() {
        // Quake BSP uses negative values for leaf contents
        assert!(CONTENTS_EMPTY < 0);
        assert!(CONTENTS_SOLID < 0);
        assert!(CONTENTS_WATER < 0);
        assert!(CONTENTS_SLIME < 0);
        assert!(CONTENTS_LAVA < 0);
        assert!(CONTENTS_SKY < 0);
    }

    #[test]
    fn bsp_contents_all_distinct() {
        let values = [
            CONTENTS_EMPTY,
            CONTENTS_SOLID,
            CONTENTS_WATER,
            CONTENTS_SLIME,
            CONTENTS_LAVA,
            CONTENTS_SKY,
        ];
        for i in 0..values.len() {
            for j in (i + 1)..values.len() {
                assert_ne!(values[i], values[j], "contents {i} and {j} collide");
            }
        }
    }

    // ── Renderer ────────────────────────────────────────────────────

    #[test]
    fn screen_dimensions_positive() {
        assert!(SCREENWIDTH > 0);
        assert!(SCREENHEIGHT > 0);
    }

    #[test]
    fn fov_is_90_degrees() {
        assert_eq!(FOV_DEGREES, 90.0);
    }

    #[test]
    fn near_clip_positive() {
        assert!(NEAR_CLIP > 0.0);
    }

    // ── Player constants ────────────────────────────────────────────

    #[test]
    fn player_view_below_full_height() {
        assert!(PLAYER_VIEW_HEIGHT < PLAYER_HEIGHT);
    }

    #[test]
    fn player_step_below_view_height() {
        assert!(STEPSIZE < PLAYER_VIEW_HEIGHT);
    }

    #[test]
    fn player_speeds_positive() {
        assert!(PLAYER_MOVE_SPEED > 0.0);
        assert!(PLAYER_STRAFE_SPEED > 0.0);
        assert!(PLAYER_JUMP_VELOCITY > 0.0);
    }

    #[test]
    fn player_run_multiplier_greater_than_one() {
        assert!(PLAYER_RUN_MULT > 1.0);
    }

    #[test]
    fn player_radius_positive() {
        assert!(PLAYER_RADIUS > 0.0);
    }

    // ── Physics ─────────────────────────────────────────────────────

    #[test]
    fn gravity_positive() {
        assert!(SV_GRAVITY > 0.0);
    }

    #[test]
    fn friction_positive() {
        assert!(SV_FRICTION > 0.0);
    }

    #[test]
    fn max_velocity_exceeds_move_speed() {
        assert!(SV_MAXVELOCITY > PLAYER_MOVE_SPEED);
    }

    #[test]
    fn stop_speed_below_move_speed() {
        assert!(SV_STOPSPEED < PLAYER_MOVE_SPEED);
    }

    #[test]
    fn tick_rate_quake_72hz() {
        assert_eq!(TICKRATE, 72);
    }

    #[test]
    fn tick_duration_reciprocal() {
        let expected = 1.0 / 72.0;
        assert!((TICK_SECS - expected).abs() < 1e-10);
    }

    // ── Colors ──────────────────────────────────────────────────────

    #[test]
    fn wall_colors_has_eight_entries() {
        assert_eq!(WALL_COLORS.len(), 8);
    }

    #[test]
    fn color_arrays_are_rgb_triples() {
        // Each entry must be [r, g, b] with 3 elements (enforced by type, but verify values are sane)
        for (i, color) in WALL_COLORS.iter().enumerate() {
            assert!(color.iter().any(|&c| c > 0), "wall color {i} is pure black");
        }
        assert!(SKY_TOP.iter().any(|&c| c > 0));
        assert!(SKY_BOTTOM.iter().any(|&c| c > 0));
        assert!(FLOOR_NEAR.iter().any(|&c| c > 0));
        assert!(FLOOR_FAR.iter().any(|&c| c > 0));
        assert!(CEILING_COLOR.iter().any(|&c| c > 0));
        assert!(FOG_COLOR.iter().any(|&c| c > 0));
    }

    // ── Fog ─────────────────────────────────────────────────────────

    #[test]
    fn fog_start_before_end() {
        assert!(FOG_START < FOG_END);
    }

    #[test]
    fn fog_distances_positive() {
        assert!(FOG_START > 0.0);
        assert!(FOG_END > 0.0);
    }

    // ── Lightmaps ───────────────────────────────────────────────────

    #[test]
    fn max_lightmaps_positive() {
        assert!(MAXLIGHTMAPS > 0);
    }
}
