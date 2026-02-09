//! Player state and movement for the Doom engine.

use super::constants::*;
use super::geometry;
use super::map::DoomMap;

/// Player state for the Doom engine.
#[derive(Debug, Clone)]
pub struct Player {
    /// X position in map units.
    pub x: f32,
    /// Y position in map units.
    pub y: f32,
    /// View height (eye level above floor).
    pub view_z: f32,
    /// Floor height at player's position.
    pub floor_z: f32,
    /// Yaw angle in radians.
    pub angle: f32,
    /// Pitch angle in radians (look up/down, not in original Doom).
    pub pitch: f32,
    /// Momentum X.
    pub mom_x: f32,
    /// Momentum Y.
    pub mom_y: f32,
    /// Vertical velocity.
    pub mom_z: f32,
    /// Whether player is on the ground.
    pub on_ground: bool,
    /// Walk cycle phase (for view bob).
    pub bob_phase: f32,
    /// Walk cycle intensity.
    pub bob_amount: f32,
    /// Health (0-200).
    pub health: i32,
    /// Armor (0-200).
    pub armor: i32,
    /// Whether running.
    pub running: bool,
    /// Noclip mode.
    pub noclip: bool,
    /// God mode.
    pub god_mode: bool,
    /// Current sector index.
    pub sector: usize,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            view_z: PLAYER_VIEW_HEIGHT,
            floor_z: 0.0,
            angle: 0.0,
            pitch: 0.0,
            mom_x: 0.0,
            mom_y: 0.0,
            mom_z: 0.0,
            on_ground: true,
            bob_phase: 0.0,
            bob_amount: 0.0,
            health: 100,
            armor: 0,
            running: false,
            noclip: false,
            god_mode: false,
            sector: 0,
        }
    }
}

impl Player {
    /// Spawn the player at the given map position.
    pub fn spawn(&mut self, x: f32, y: f32, angle: f32) {
        self.x = x;
        self.y = y;
        self.angle = angle;
        self.mom_x = 0.0;
        self.mom_y = 0.0;
        self.mom_z = 0.0;
        self.on_ground = true;
        self.bob_phase = 0.0;
        self.bob_amount = 0.0;
    }

    /// Apply thrust in a direction.
    pub fn thrust(&mut self, angle: f32, speed: f32) {
        let mult = if self.running { PLAYER_RUN_MULT } else { 1.0 };
        self.mom_x += angle.cos() * speed * mult;
        self.mom_y += angle.sin() * speed * mult;
    }

    /// Move forward (positive) or backward (negative).
    pub fn move_forward(&mut self, amount: f32) {
        self.thrust(self.angle, amount * PLAYER_MOVE_SPEED);
    }

    /// Strafe right (positive) or left (negative).
    pub fn strafe(&mut self, amount: f32) {
        let strafe_angle = self.angle - std::f32::consts::FRAC_PI_2;
        self.thrust(strafe_angle, amount * PLAYER_STRAFE_SPEED);
    }

    /// Rotate view (yaw and pitch).
    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.angle += yaw_delta;
        // Keep angle in [0, 2π)
        self.angle = self.angle.rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + pitch_delta).clamp(-1.2, 1.2);
    }

    /// Run a physics tick: apply friction, gravity, collision, sector height.
    pub fn tick(&mut self, map: &DoomMap) {
        // Apply friction
        self.mom_x *= PLAYER_FRICTION;
        self.mom_y *= PLAYER_FRICTION;

        // Clamp momentum
        let speed = (self.mom_x * self.mom_x + self.mom_y * self.mom_y).sqrt();
        if speed > PLAYER_MAX_MOVE {
            let scale = PLAYER_MAX_MOVE / speed;
            self.mom_x *= scale;
            self.mom_y *= scale;
        }

        // Kill tiny momentum
        if speed < 0.1 {
            self.mom_x = 0.0;
            self.mom_y = 0.0;
        }

        // Try to move
        if self.noclip {
            self.x += self.mom_x;
            self.y += self.mom_y;
        } else {
            self.try_move(map, self.x + self.mom_x, self.y + self.mom_y);
        }

        // Update sector
        self.sector = map.point_in_subsector(self.x, self.y);

        // Get floor height at new position
        if let Some(sector) = map.point_sector(self.x, self.y) {
            let target_floor = sector.floor_height;
            if self.on_ground || self.floor_z > target_floor + PLAYER_STEP_HEIGHT {
                // Step up stairs or drop
                if target_floor <= self.floor_z + PLAYER_STEP_HEIGHT {
                    self.floor_z = target_floor;
                }
            }
        }

        // Gravity
        if !self.on_ground {
            self.mom_z -= GRAVITY;
        }

        // Apply vertical movement
        self.view_z += self.mom_z;
        let target_z = self.floor_z + PLAYER_VIEW_HEIGHT;
        if self.view_z <= target_z {
            self.view_z = target_z;
            self.mom_z = 0.0;
            self.on_ground = true;
        } else {
            self.on_ground = false;
        }

        // View bob
        if speed > 0.5 && self.on_ground {
            self.bob_phase += speed * 0.08;
            self.bob_amount = (self.bob_amount + 0.1).min(1.0);
        } else {
            self.bob_amount *= 0.9;
        }
    }

    /// Try to move to a new position with collision detection.
    fn try_move(&mut self, map: &DoomMap, new_x: f32, new_y: f32) {
        // Check collision against blocking linedefs
        let mut blocked_x = false;
        let mut blocked_y = false;

        for linedef in &map.linedefs {
            if !linedef.is_blocking() && linedef.is_two_sided() {
                // Two-sided non-blocking: check step height
                if let (Some(front), Some(back)) = (
                    linedef.front_sector(&map.sidedefs),
                    linedef.back_sector(&map.sidedefs),
                ) {
                    let front_floor = map.sectors[front].floor_height;
                    let back_floor = map.sectors[back].floor_height;
                    let front_ceil = map.sectors[front].ceiling_height;
                    let back_ceil = map.sectors[back].ceiling_height;

                    let step = (front_floor - back_floor).abs();
                    let min_ceil = front_ceil.min(back_ceil);

                    // Check if gap is passable
                    if step > PLAYER_STEP_HEIGHT || min_ceil - self.floor_z < PLAYER_HEIGHT {
                        // Impassable two-sided line: check per-axis like solid walls
                        let x1 = map.vertices[linedef.v1].x;
                        let y1 = map.vertices[linedef.v1].y;
                        let x2 = map.vertices[linedef.v2].x;
                        let y2 = map.vertices[linedef.v2].y;

                        if !blocked_x
                            && geometry::circle_intersects_segment(
                                new_x,
                                self.y,
                                PLAYER_RADIUS,
                                x1,
                                y1,
                                x2,
                                y2,
                            )
                        {
                            blocked_x = true;
                        }
                        if !blocked_y
                            && geometry::circle_intersects_segment(
                                self.x,
                                new_y,
                                PLAYER_RADIUS,
                                x1,
                                y1,
                                x2,
                                y2,
                            )
                        {
                            blocked_y = true;
                        }
                    }
                }
                continue;
            }

            if !linedef.is_blocking() {
                continue;
            }

            let x1 = map.vertices[linedef.v1].x;
            let y1 = map.vertices[linedef.v1].y;
            let x2 = map.vertices[linedef.v2].x;
            let y2 = map.vertices[linedef.v2].y;

            // Check X-only movement
            if !blocked_x
                && geometry::circle_intersects_segment(new_x, self.y, PLAYER_RADIUS, x1, y1, x2, y2)
            {
                blocked_x = true;
            }

            // Check Y-only movement
            if !blocked_y
                && geometry::circle_intersects_segment(self.x, new_y, PLAYER_RADIUS, x1, y1, x2, y2)
            {
                blocked_y = true;
            }
        }

        if !blocked_x {
            self.x = new_x;
        }
        if !blocked_y {
            self.y = new_y;
        }
    }

    /// Get the view bob offset for the current frame.
    pub fn bob_offset(&self) -> f32 {
        self.bob_amount * (self.bob_phase * 2.0).sin() * 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_player() {
        let p = Player::default();
        assert_eq!(p.health, 100);
        assert!(p.on_ground);
        assert!((p.view_z - PLAYER_VIEW_HEIGHT).abs() < 0.01);
    }

    #[test]
    fn player_spawn() {
        let mut p = Player::default();
        p.spawn(100.0, 200.0, 1.5);
        assert!((p.x - 100.0).abs() < 0.01);
        assert!((p.y - 200.0).abs() < 0.01);
        assert!((p.angle - 1.5).abs() < 0.01);
    }

    #[test]
    fn player_look_clamps_pitch() {
        let mut p = Player::default();
        p.look(0.0, 10.0);
        assert!(p.pitch <= 1.2);
        p.look(0.0, -20.0);
        assert!(p.pitch >= -1.2);
    }

    #[test]
    fn player_thrust_adds_momentum() {
        let mut p = Player::default();
        p.thrust(0.0, 5.0); // thrust right
        assert!(p.mom_x > 0.0, "x momentum should increase");
        assert!(p.mom_y.abs() < 0.01, "y momentum should be near zero");
    }

    #[test]
    fn player_thrust_running_multiplier() {
        let mut p1 = Player::default();
        let mut p2 = Player {
            running: true,
            ..Default::default()
        };
        p1.thrust(0.0, 5.0);
        p2.thrust(0.0, 5.0);
        assert!(
            p2.mom_x > p1.mom_x,
            "running player should have more momentum"
        );
        assert!(
            (p2.mom_x / p1.mom_x - PLAYER_RUN_MULT).abs() < 0.01,
            "running should apply run multiplier"
        );
    }

    #[test]
    fn move_forward_uses_angle() {
        let mut p = Player {
            angle: std::f32::consts::FRAC_PI_2, // facing up (y+)
            ..Default::default()
        };
        p.move_forward(1.0);
        assert!(
            p.mom_y.abs() > p.mom_x.abs(),
            "forward at pi/2 should mostly add y momentum"
        );
    }

    #[test]
    fn strafe_perpendicular_to_facing() {
        let mut p = Player::default(); // facing right
        p.strafe(1.0); // strafe right should be downward (angle - pi/2)
        assert!(
            p.mom_y.abs() > p.mom_x.abs(),
            "strafing should mostly add perpendicular momentum"
        );
    }

    #[test]
    fn look_wraps_yaw() {
        let mut p = Player::default();
        p.look(std::f32::consts::TAU + 0.5, 0.0);
        assert!(p.angle >= 0.0 && p.angle < std::f32::consts::TAU);
    }

    #[test]
    fn spawn_resets_momentum() {
        let mut p = Player {
            mom_x: 10.0,
            mom_y: 20.0,
            mom_z: 5.0,
            bob_phase: 3.0,
            ..Default::default()
        };
        p.spawn(50.0, 60.0, 1.0);
        assert_eq!(p.mom_x, 0.0);
        assert_eq!(p.mom_y, 0.0);
        assert_eq!(p.mom_z, 0.0);
        assert_eq!(p.bob_phase, 0.0);
        assert!(p.on_ground);
    }

    #[test]
    fn bob_offset_zero_when_still() {
        let p = Player::default();
        // bob_amount is 0 by default
        assert_eq!(p.bob_offset(), 0.0);
    }

    #[test]
    fn bob_offset_nonzero_with_bob_amount() {
        let p = Player {
            bob_amount: 1.0,
            bob_phase: std::f32::consts::FRAC_PI_4, // sin(pi/2) = 1.0
            ..Default::default()
        };
        let offset = p.bob_offset();
        assert!(
            offset.abs() > 0.0,
            "bob_offset should be nonzero when bob_amount and phase are set"
        );
    }

    #[test]
    fn default_player_values() {
        let p = Player::default();
        assert_eq!(p.armor, 0);
        assert!(!p.running);
        assert!(!p.noclip);
        assert!(!p.god_mode);
        assert_eq!(p.sector, 0);
        assert_eq!(p.floor_z, 0.0);
    }

    // --- tick() physics ---

    /// Minimal empty map for friction/gravity-only tick tests.
    fn empty_map() -> DoomMap {
        use super::super::map::*;
        DoomMap {
            name: "EMPTY".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![SubSector {
                num_segs: 0,
                first_seg: 0,
            }],
            nodes: vec![],
            things: vec![],
        }
    }

    #[test]
    fn tick_applies_friction() {
        let map = empty_map();
        let mut p = Player::default();
        p.mom_x = 10.0;
        p.mom_y = 5.0;

        p.tick(&map);

        // Friction = 0.90625, so momentum should be reduced
        assert!(
            (p.mom_x - 10.0 * PLAYER_FRICTION).abs() < 0.01,
            "x momentum should be reduced by friction"
        );
        assert!(
            (p.mom_y - 5.0 * PLAYER_FRICTION).abs() < 0.01,
            "y momentum should be reduced by friction"
        );
    }

    #[test]
    fn tick_clamps_excessive_speed() {
        let map = empty_map();
        let mut p = Player::default();
        // Set very high momentum (well above PLAYER_MAX_MOVE)
        p.mom_x = 100.0;
        p.mom_y = 100.0;

        p.tick(&map);

        let speed = (p.mom_x * p.mom_x + p.mom_y * p.mom_y).sqrt();
        // After friction and clamping, speed should be at most PLAYER_MAX_MOVE
        // (friction is applied first: 100*0.90625=90.625 per axis, then clamped)
        assert!(
            speed <= PLAYER_MAX_MOVE + 0.01,
            "speed {speed} should be clamped to PLAYER_MAX_MOVE {PLAYER_MAX_MOVE}"
        );
    }

    #[test]
    fn tick_kills_tiny_momentum() {
        let map = empty_map();
        let mut p = Player::default();
        // Set very small momentum (below the 0.1 threshold after friction)
        p.mom_x = 0.05;
        p.mom_y = 0.05;

        p.tick(&map);

        // After friction: 0.05 * 0.90625 = 0.0453125
        // Speed = sqrt(0.0453^2 + 0.0453^2) ≈ 0.064 < 0.1 → killed
        assert_eq!(p.mom_x, 0.0, "tiny x momentum should be killed");
        assert_eq!(p.mom_y, 0.0, "tiny y momentum should be killed");
    }

    #[test]
    fn tick_applies_noclip_movement() {
        let map = empty_map();
        let mut p = Player::default();
        p.noclip = true;
        p.mom_x = 5.0;
        p.mom_y = 3.0;

        let old_x = p.x;
        let old_y = p.y;
        p.tick(&map);

        // In noclip, position changes directly by (friction-reduced) momentum
        let expected_dx = 5.0 * PLAYER_FRICTION;
        let expected_dy = 3.0 * PLAYER_FRICTION;
        assert!(
            (p.x - old_x - expected_dx).abs() < 0.01,
            "noclip should move x by momentum"
        );
        assert!(
            (p.y - old_y - expected_dy).abs() < 0.01,
            "noclip should move y by momentum"
        );
    }

    #[test]
    fn tick_gravity_when_airborne() {
        let map = empty_map();
        let mut p = Player::default();
        p.on_ground = false;
        p.view_z = PLAYER_VIEW_HEIGHT + 50.0; // well above floor
        p.mom_z = 0.0;

        p.tick(&map);

        // Gravity should pull mom_z negative
        assert!(
            p.mom_z < 0.0,
            "gravity should add negative vertical velocity"
        );
        assert!((p.mom_z - (-GRAVITY)).abs() < 0.01);
    }

    #[test]
    fn tick_landing_resets_on_ground() {
        let map = empty_map();
        let mut p = Player::default();
        p.on_ground = false;
        // Set view_z just barely above target (floor_z + VIEW_HEIGHT)
        p.view_z = PLAYER_VIEW_HEIGHT + 0.5;
        p.mom_z = -1.0;

        p.tick(&map);

        // After applying mom_z, view_z would drop below floor → snap to floor and land
        assert!(p.on_ground, "player should land when view_z reaches floor");
        assert_eq!(p.mom_z, 0.0, "vertical momentum should reset on landing");
        assert!(
            (p.view_z - (p.floor_z + PLAYER_VIEW_HEIGHT)).abs() < 0.01,
            "view_z should snap to floor + view height"
        );
    }

    #[test]
    fn tick_bob_increases_while_moving() {
        let map = empty_map();
        let mut p = Player::default();
        p.mom_x = 5.0; // enough speed for bob (>0.5 after friction)

        assert_eq!(p.bob_amount, 0.0);
        p.tick(&map);

        // Speed after friction: 5.0 * 0.90625 = 4.53, well above 0.5
        assert!(
            p.bob_amount > 0.0,
            "bob_amount should increase while moving on ground"
        );
        assert!(
            p.bob_phase > 0.0,
            "bob_phase should advance while moving on ground"
        );
    }

    #[test]
    fn tick_bob_decays_when_still() {
        let map = empty_map();
        let mut p = Player::default();
        p.bob_amount = 0.8;
        p.mom_x = 0.0;
        p.mom_y = 0.0;

        p.tick(&map);

        // No speed → bob_amount *= 0.9
        assert!(
            (p.bob_amount - 0.8 * 0.9).abs() < 0.01,
            "bob_amount should decay when still"
        );
    }

    #[test]
    fn tick_bob_capped_at_one() {
        let map = empty_map();
        let mut p = Player::default();
        p.bob_amount = 1.0;
        p.mom_x = 5.0; // moving fast enough

        p.tick(&map);

        assert!(p.bob_amount <= 1.0, "bob_amount should be capped at 1.0");
    }

    #[test]
    fn tick_no_gravity_on_ground() {
        let map = empty_map();
        let mut p = Player::default();
        assert!(p.on_ground);
        p.mom_z = 0.0;

        p.tick(&map);

        assert_eq!(p.mom_z, 0.0, "gravity should not apply when on ground");
    }

    // --- try_move() collision ---

    /// Build a simple room with 4 blocking walls where the player can collide.
    fn boxed_room_map() -> DoomMap {
        use super::super::map::*;
        use super::super::wad_types::ML_BLOCKING;
        let vertices = vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 256.0, y: 0.0 },
            Vertex { x: 256.0, y: 256.0 },
            Vertex { x: 0.0, y: 256.0 },
        ];
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
        let linedefs = vec![
            LineDef {
                v1: 0,
                v2: 1,
                flags: ML_BLOCKING,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 1,
                v2: 2,
                flags: ML_BLOCKING,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 2,
                v2: 3,
                flags: ML_BLOCKING,
                special: 0,
                tag: 0,
                front_sidedef: Some(0),
                back_sidedef: None,
            },
            LineDef {
                v1: 3,
                v2: 0,
                flags: ML_BLOCKING,
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
        DoomMap {
            name: "BOX".into(),
            vertices,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors: vec![SubSector {
                num_segs: 4,
                first_seg: 0,
            }],
            nodes: vec![],
            things: vec![],
        }
    }

    #[test]
    fn collision_blocks_movement_into_wall() {
        let map = boxed_room_map();
        let mut p = Player::default();
        // Place player near bottom wall (y=0), try to move through it
        p.x = 128.0;
        p.y = PLAYER_RADIUS + 1.0; // just inside the room
        p.mom_x = 0.0;
        p.mom_y = -50.0; // push hard into the wall at y=0
        p.noclip = false;

        p.tick(&map);

        // Y should not go below 0 (wall blocks), but might slide along X
        assert!(
            p.y >= 0.0,
            "player should not pass through blocking wall, y={}",
            p.y
        );
    }

    #[test]
    fn collision_allows_free_movement_in_center() {
        let map = boxed_room_map();
        let mut p = Player::default();
        p.x = 128.0;
        p.y = 128.0;
        p.mom_x = 5.0;
        p.mom_y = 3.0;
        p.noclip = false;

        let old_x = p.x;
        let old_y = p.y;
        p.tick(&map);

        // In center of room, should move freely
        assert!(
            (p.x - old_x).abs() > 1.0,
            "player should move freely in center (x)"
        );
        assert!(
            (p.y - old_y).abs() > 1.0,
            "player should move freely in center (y)"
        );
    }

    #[test]
    fn noclip_ignores_walls() {
        let map = boxed_room_map();
        let mut p = Player::default();
        p.x = 128.0;
        p.y = 10.0;
        p.mom_y = -50.0;
        p.noclip = true;

        p.tick(&map);

        // With noclip, player should pass through the wall
        assert!(
            p.y < 0.0,
            "noclip player should pass through walls, y={}",
            p.y
        );
    }

    // --- move_forward/strafe edge cases ---

    #[test]
    fn move_forward_negative_goes_backward() {
        let mut p = Player::default(); // angle=0 → forward is +X
        p.move_forward(-1.0);
        assert!(
            p.mom_x < 0.0,
            "negative forward should add negative x momentum"
        );
    }

    #[test]
    fn strafe_negative_goes_left() {
        let mut p = Player::default(); // angle=0 → strafe right is -Y
        p.strafe(-1.0);
        // Strafe left (negative) at angle=0 should push toward +Y
        assert!(p.mom_y > 0.0, "negative strafe should push in +Y direction");
    }

    // --- look edge cases ---

    #[test]
    fn look_negative_yaw_wraps_to_positive() {
        let mut p = Player::default();
        p.look(-0.5, 0.0);
        assert!(
            p.angle >= 0.0 && p.angle < std::f32::consts::TAU,
            "angle should wrap to [0, TAU)"
        );
        assert!(
            (p.angle - (std::f32::consts::TAU - 0.5)).abs() < 0.01,
            "negative yaw should wrap correctly"
        );
    }

    #[test]
    fn look_full_rotation_returns_to_zero() {
        let mut p = Player::default();
        p.look(std::f32::consts::TAU, 0.0);
        assert!(
            p.angle.abs() < 0.01,
            "full rotation should return to ~0, got {}",
            p.angle
        );
    }

    #[test]
    fn pitch_clamp_symmetric() {
        let mut p = Player::default();
        p.look(0.0, 100.0);
        assert!((p.pitch - 1.2).abs() < 0.01, "pitch should clamp at +1.2");

        let mut p2 = Player::default();
        p2.look(0.0, -100.0);
        assert!(
            (p2.pitch - (-1.2)).abs() < 0.01,
            "pitch should clamp at -1.2"
        );
    }

    // --- Multiple ticks converge ---

    #[test]
    fn repeated_ticks_friction_converges_to_zero() {
        let map = empty_map();
        let mut p = Player::default();
        p.mom_x = 20.0;
        p.mom_y = 15.0;

        for _ in 0..100 {
            p.tick(&map);
        }

        // After 100 ticks of friction with no input, momentum should be ~0
        assert!(
            p.mom_x.abs() < 0.01,
            "x momentum should converge to 0 after many ticks"
        );
        assert!(
            p.mom_y.abs() < 0.01,
            "y momentum should converge to 0 after many ticks"
        );
    }
}
