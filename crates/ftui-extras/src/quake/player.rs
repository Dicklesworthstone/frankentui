//! Player state and movement for the Quake engine.
//!
//! Ported from Quake 1 sv_move.c / sv_phys.c (id Software GPL).

use super::constants::*;
use super::map::QuakeMap;

/// Player state.
#[derive(Debug, Clone)]
pub struct Player {
    /// 3D position.
    pub pos: [f32; 3],
    /// Velocity.
    pub vel: [f32; 3],
    /// Yaw angle in radians.
    pub yaw: f32,
    /// Pitch angle in radians.
    pub pitch: f32,
    /// Whether player is on the ground.
    pub on_ground: bool,
    /// Walk bob phase.
    pub bob_phase: f32,
    /// Walk bob intensity.
    pub bob_amount: f32,
    /// Whether running.
    pub running: bool,
    /// Noclip mode.
    pub noclip: bool,
    /// Health.
    pub health: i32,
    /// Armor.
    pub armor: i32,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            pos: [0.0, 0.0, 0.0],
            vel: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            bob_phase: 0.0,
            bob_amount: 0.0,
            running: false,
            noclip: false,
            health: 100,
            armor: 0,
        }
    }
}

impl Player {
    /// Spawn at a position with an angle.
    pub fn spawn(&mut self, x: f32, y: f32, z: f32, yaw: f32) {
        self.pos = [x, y, z];
        self.vel = [0.0, 0.0, 0.0];
        self.yaw = yaw;
        self.pitch = 0.0;
        self.on_ground = true;
        self.bob_phase = 0.0;
        self.bob_amount = 0.0;
    }

    /// Get the eye position (pos + view height).
    pub fn eye_pos(&self) -> [f32; 3] {
        [
            self.pos[0],
            self.pos[1],
            self.pos[2] + PLAYER_VIEW_HEIGHT + self.bob_offset(),
        ]
    }

    /// Get the forward direction vector.
    pub fn forward(&self) -> [f32; 3] {
        let cp = self.pitch.cos();
        [self.yaw.cos() * cp, self.yaw.sin() * cp, -self.pitch.sin()]
    }

    /// Get the right direction vector.
    pub fn right(&self) -> [f32; 3] {
        let r = self.yaw - std::f32::consts::FRAC_PI_2;
        [r.cos(), r.sin(), 0.0]
    }

    /// Get the up direction vector.
    pub fn up(&self) -> [f32; 3] {
        let fwd = self.forward();
        let right = self.right();
        cross(right, fwd)
    }

    /// Move forward/backward.
    pub fn move_forward(&mut self, amount: f32) {
        let speed = if self.running {
            PLAYER_MOVE_SPEED * PLAYER_RUN_MULT
        } else {
            PLAYER_MOVE_SPEED
        };
        let cy = self.yaw.cos();
        let sy = self.yaw.sin();
        self.vel[0] += cy * amount * speed;
        self.vel[1] += sy * amount * speed;
    }

    /// Strafe left/right.
    pub fn strafe(&mut self, amount: f32) {
        let speed = if self.running {
            PLAYER_STRAFE_SPEED * PLAYER_RUN_MULT
        } else {
            PLAYER_STRAFE_SPEED
        };
        let r = self.yaw - std::f32::consts::FRAC_PI_2;
        self.vel[0] += r.cos() * amount * speed;
        self.vel[1] += r.sin() * amount * speed;
    }

    /// Look (yaw and pitch).
    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw += yaw_delta;
        self.yaw = self.yaw.rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + pitch_delta).clamp(-1.4, 1.4);
    }

    /// Jump.
    pub fn jump(&mut self) {
        if self.on_ground {
            self.vel[2] = PLAYER_JUMP_VELOCITY;
            self.on_ground = false;
        }
    }

    /// Run a physics tick (called at TICKRATE Hz).
    pub fn tick(&mut self, map: &QuakeMap, dt: f32) {
        // Apply ground friction (from Quake SV_Friction)
        if self.on_ground {
            let speed = (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1]).sqrt();
            if speed > 0.0 {
                let control = if speed < SV_STOPSPEED {
                    SV_STOPSPEED
                } else {
                    speed
                };
                let drop = control * SV_FRICTION * dt;
                let new_speed = ((speed - drop) / speed).max(0.0);
                self.vel[0] *= new_speed;
                self.vel[1] *= new_speed;
            }
        }

        // Clamp velocity
        for v in &mut self.vel {
            *v = v.clamp(-SV_MAXVELOCITY, SV_MAXVELOCITY);
        }

        // Apply gravity
        if !self.on_ground {
            self.vel[2] -= SV_GRAVITY * dt;
        }

        // Try to move
        let new_pos = [
            self.pos[0] + self.vel[0] * dt,
            self.pos[1] + self.vel[1] * dt,
            self.pos[2] + self.vel[2] * dt,
        ];

        if self.noclip {
            self.pos = new_pos;
        } else {
            self.try_move(map, new_pos, dt);
        }

        // Ground check: find floor height at current position (Z-aware to avoid
        // teleporting up to platforms that are far above the player).
        let floor_z = map.supportive_floor_at(self.pos[0], self.pos[1], self.pos[2]);
        if self.pos[2] <= floor_z || ((self.pos[2] - floor_z).abs() < 1.0 && self.vel[2] <= 0.0) {
            self.pos[2] = floor_z;
            self.vel[2] = 0.0;
            self.on_ground = true;
        } else {
            self.on_ground = false;
        }

        // Ceiling check
        let ceil_z = map.ceiling_height_at(self.pos[0], self.pos[1]);
        if self.pos[2] + PLAYER_HEIGHT > ceil_z {
            self.pos[2] = ceil_z - PLAYER_HEIGHT;
            if self.vel[2] > 0.0 {
                self.vel[2] = 0.0;
            }
        }

        // View bob
        let ground_speed = (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1]).sqrt();
        if ground_speed > 10.0 && self.on_ground {
            self.bob_phase += ground_speed * dt * 0.015;
            self.bob_amount = (self.bob_amount + dt * 4.0).min(1.0);
        } else {
            self.bob_amount *= 1.0 - dt * 6.0;
            if self.bob_amount < 0.01 {
                self.bob_amount = 0.0;
            }
        }
    }

    /// Try to move with collision detection against the map.
    fn try_move(&mut self, map: &QuakeMap, new_pos: [f32; 3], _dt: f32) {
        // Try full move
        if !map.point_in_solid(new_pos[0], new_pos[1], new_pos[2], PLAYER_RADIUS) {
            // Check step-up
            let floor_z = map.floor_height_at(new_pos[0], new_pos[1]);
            if new_pos[2] >= floor_z || (floor_z - self.pos[2]) <= STEPSIZE {
                self.pos = new_pos;
                return;
            }
        }

        // Slide along X axis
        let slide_x = [new_pos[0], self.pos[1], self.pos[2]];
        if !map.point_in_solid(slide_x[0], slide_x[1], slide_x[2], PLAYER_RADIUS) {
            self.pos[0] = slide_x[0];
        } else {
            self.vel[0] = 0.0;
        }

        // Slide along Y axis
        let slide_y = [self.pos[0], new_pos[1], self.pos[2]];
        if !map.point_in_solid(slide_y[0], slide_y[1], slide_y[2], PLAYER_RADIUS) {
            self.pos[1] = slide_y[1];
        } else {
            self.vel[1] = 0.0;
        }

        // Vertical
        if !map.point_in_solid(self.pos[0], self.pos[1], new_pos[2], PLAYER_RADIUS) {
            self.pos[2] = new_pos[2];
        } else {
            self.vel[2] = 0.0;
        }
    }

    /// Get view bob offset.
    pub fn bob_offset(&self) -> f32 {
        self.bob_amount * (self.bob_phase * 2.0).sin() * 1.5
    }
}

/// Cross product of two 3D vectors.
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_player() {
        let p = Player::default();
        assert_eq!(p.health, 100);
        assert!(p.on_ground);
    }

    #[test]
    fn player_spawn() {
        let mut p = Player::default();
        p.spawn(100.0, 200.0, 50.0, 1.5);
        assert!((p.pos[0] - 100.0).abs() < 0.01);
        assert!((p.pos[1] - 200.0).abs() < 0.01);
        assert!((p.pos[2] - 50.0).abs() < 0.01);
    }

    #[test]
    fn look_clamps_pitch() {
        let mut p = Player::default();
        p.look(0.0, 10.0);
        assert!(p.pitch <= 1.4);
        p.look(0.0, -20.0);
        assert!(p.pitch >= -1.4);
    }

    #[test]
    fn eye_pos_above_feet() {
        let p = Player::default();
        let eye = p.eye_pos();
        assert!(eye[2] > p.pos[2]);
    }

    #[test]
    fn forward_at_zero_yaw_is_x_axis() {
        let p = Player::default();
        let fwd = p.forward();
        assert!((fwd[0] - 1.0).abs() < 0.01);
        assert!(fwd[1].abs() < 0.01);
        assert!(fwd[2].abs() < 0.01);
    }

    #[test]
    fn right_perpendicular_to_forward() {
        let p = Player::default();
        let fwd = p.forward();
        let right = p.right();
        let dot = fwd[0] * right[0] + fwd[1] * right[1] + fwd[2] * right[2];
        assert!(
            dot.abs() < 0.01,
            "forward and right should be perpendicular, dot={dot}"
        );
    }

    #[test]
    fn move_forward_adds_velocity() {
        let mut p = Player::default();
        p.move_forward(1.0);
        let speed_sq = p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1];
        assert!(speed_sq > 0.0, "move_forward should add velocity");
    }

    #[test]
    fn strafe_adds_lateral_velocity() {
        let mut p = Player::default();
        p.strafe(1.0);
        // At yaw=0, strafe should add velocity in y direction
        let speed_sq = p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1];
        assert!(speed_sq > 0.0, "strafe should add velocity");
    }

    #[test]
    fn jump_only_from_ground() {
        let mut p = Player::default();
        assert!(p.on_ground);
        p.jump();
        assert!(p.vel[2] > 0.0);
        assert!(!p.on_ground);
        // Jump again while airborne should do nothing
        let vel_z = p.vel[2];
        p.jump();
        assert!((p.vel[2] - vel_z).abs() < 0.01, "should not double-jump");
    }

    #[test]
    fn bob_offset_zero_when_no_bob() {
        let p = Player::default();
        assert!((p.bob_offset()).abs() < 0.001);
    }

    #[test]
    fn running_increases_move_speed() {
        let mut p1 = Player::default();
        let mut p2 = Player {
            running: true,
            ..Player::default()
        };
        p1.move_forward(1.0);
        p2.move_forward(1.0);
        let speed1 = p1.vel[0] * p1.vel[0] + p1.vel[1] * p1.vel[1];
        let speed2 = p2.vel[0] * p2.vel[0] + p2.vel[1] * p2.vel[1];
        assert!(speed2 > speed1, "running should increase speed");
    }

    #[test]
    fn look_yaw_wraps_around() {
        let mut p = Player::default();
        p.look(std::f32::consts::TAU + 0.5, 0.0);
        assert!(p.yaw >= 0.0 && p.yaw < std::f32::consts::TAU);
    }

    #[test]
    fn spawn_resets_velocity_and_state() {
        let mut p = Player {
            vel: [100.0, 200.0, 300.0],
            pitch: 0.5,
            on_ground: false,
            bob_phase: 5.0,
            bob_amount: 1.0,
            ..Player::default()
        };
        p.spawn(10.0, 20.0, 30.0, 1.0);
        assert_eq!(p.vel, [0.0, 0.0, 0.0]);
        assert_eq!(p.pitch, 0.0);
        assert!(p.on_ground);
        assert_eq!(p.bob_phase, 0.0);
        assert_eq!(p.bob_amount, 0.0);
        assert!((p.yaw - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spawn_preserves_health_and_armor() {
        let mut p = Player {
            health: 50,
            armor: 75,
            ..Player::default()
        };
        p.spawn(0.0, 0.0, 0.0, 0.0);
        // spawn doesn't reset health/armor
        assert_eq!(p.health, 50);
        assert_eq!(p.armor, 75);
    }

    #[test]
    fn eye_pos_includes_view_height() {
        let p = Player {
            pos: [10.0, 20.0, 30.0],
            ..Player::default()
        };
        let eye = p.eye_pos();
        assert_eq!(eye[0], 10.0);
        assert_eq!(eye[1], 20.0);
        // eye_z = pos_z + PLAYER_VIEW_HEIGHT + bob_offset (bob is 0 for default)
        assert!((eye[2] - (30.0 + PLAYER_VIEW_HEIGHT)).abs() < 0.01);
    }

    #[test]
    fn forward_at_90_degrees_yaw_is_y_axis() {
        let p = Player {
            yaw: std::f32::consts::FRAC_PI_2,
            ..Player::default()
        };
        let fwd = p.forward();
        assert!(fwd[0].abs() < 0.01, "x should be ~0, got {}", fwd[0]);
        assert!(
            (fwd[1] - 1.0).abs() < 0.01,
            "y should be ~1, got {}",
            fwd[1]
        );
        assert!(fwd[2].abs() < 0.01);
    }

    #[test]
    fn forward_with_pitch_tilts_z() {
        let p = Player {
            pitch: 0.5,
            ..Player::default()
        };
        let fwd = p.forward();
        // With positive pitch, z component = -sin(pitch) < 0
        assert!(
            fwd[2] < 0.0,
            "looking up (positive pitch) should tilt z negative"
        );
        // Forward should still have a positive x component at yaw=0
        assert!(fwd[0] > 0.0);
    }

    #[test]
    fn forward_is_unit_length() {
        for yaw in [0.0, 0.5, 1.0, 2.0, 4.0, 5.5] {
            for pitch in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                let p = Player {
                    yaw,
                    pitch,
                    ..Player::default()
                };
                let f = p.forward();
                let len = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();
                assert!(
                    (len - 1.0).abs() < 1e-4,
                    "forward not unit at yaw={yaw}, pitch={pitch}: len={len}"
                );
            }
        }
    }

    #[test]
    fn right_is_unit_length_and_horizontal() {
        for yaw in [0.0, 1.0, 2.5, 4.0, 6.0] {
            let p = Player {
                yaw,
                ..Player::default()
            };
            let r = p.right();
            let len = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-4,
                "right not unit at yaw={yaw}: len={len}"
            );
            assert!(r[2].abs() < 1e-6, "right should have z=0, got {}", r[2]);
        }
    }

    #[test]
    fn up_perpendicular_to_forward_and_right() {
        let p = Player {
            yaw: 0.7,
            pitch: 0.3,
            ..Player::default()
        };
        let fwd = p.forward();
        let right = p.right();
        let up = p.up();
        let dot_fwd = fwd[0] * up[0] + fwd[1] * up[1] + fwd[2] * up[2];
        let dot_right = right[0] * up[0] + right[1] * up[1] + right[2] * up[2];
        assert!(
            dot_fwd.abs() < 0.01,
            "up should be perpendicular to forward, dot={dot_fwd}"
        );
        assert!(
            dot_right.abs() < 0.01,
            "up should be perpendicular to right, dot={dot_right}"
        );
    }

    #[test]
    fn up_at_zero_pitch_is_z_axis() {
        let p = Player::default();
        let up = p.up();
        // At zero pitch, forward=[1,0,0], right=[0,-1,0], up = cross(right, fwd) = [0,0,1]
        assert!(
            up[2] > 0.5,
            "up.z should be positive at zero pitch, got {:?}",
            up
        );
    }

    #[test]
    fn tick_gravity_applies_when_airborne() {
        // Use a map with a room so floor_at works, but place player high above it
        let mut map = QuakeMap::new();
        use crate::quake::map::Room;
        map.rooms.push(Room {
            x: -500.0,
            y: -500.0,
            width: 1000.0,
            height: 1000.0,
            floor_z: 0.0,
            ceil_z: 500.0,
            light: 200.0,
        });
        let mut p = Player {
            on_ground: false,
            pos: [0.0, 0.0, 200.0], // well above the floor
            ..Player::default()
        };
        p.tick(&map, 1.0 / 72.0);
        // Gravity should have reduced vel[2] (made it negative)
        assert!(
            p.vel[2] < 0.0,
            "gravity should make z velocity negative: got {}",
            p.vel[2]
        );
    }

    #[test]
    fn tick_friction_slows_ground_player() {
        let map = crate::quake::map::generate_e1m1();
        let mut p = Player {
            // Place player at map spawn point with some ground velocity
            pos: [0.0, 0.0, 0.0],
            on_ground: true,
            vel: [200.0, 100.0, 0.0],
            ..Player::default()
        };
        let initial_speed = (p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1]).sqrt();
        p.tick(&map, 1.0 / 72.0);
        let final_speed = (p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1]).sqrt();
        assert!(
            final_speed < initial_speed,
            "friction should reduce speed: was {initial_speed}, now {final_speed}"
        );
    }

    #[test]
    fn tick_clamps_velocity_to_max() {
        let map = QuakeMap::new();
        let mut p = Player {
            vel: [5000.0, -5000.0, 5000.0],
            on_ground: false,
            pos: [0.0, 0.0, 500.0],
            noclip: true, // noclip so we don't hit collision
            ..Player::default()
        };
        p.tick(&map, 1.0 / 72.0);
        for v in &p.vel {
            assert!(
                *v >= -SV_MAXVELOCITY && *v <= SV_MAXVELOCITY,
                "velocity {v} exceeds max {SV_MAXVELOCITY}"
            );
        }
    }

    #[test]
    fn tick_noclip_moves_freely() {
        let map = QuakeMap::new();
        let mut p = Player {
            noclip: true,
            on_ground: false,
            pos: [0.0, 0.0, 100.0],
            vel: [100.0, 50.0, 0.0],
            ..Player::default()
        };
        let dt = 1.0 / 72.0;
        p.tick(&map, dt);
        // In noclip, position should change in the direction of velocity
        // (the exact value depends on friction/gravity adjustments, but pos should move)
        // Since on_ground=false, no friction. Gravity pulls z down but pos should change.
        assert!(
            p.pos[0] != 0.0 || p.pos[1] != 0.0,
            "noclip player should move"
        );
    }

    #[test]
    fn tick_view_bob_increases_with_ground_speed() {
        let map = crate::quake::map::generate_e1m1();
        let mut p = Player {
            pos: [0.0, 0.0, 0.0],
            on_ground: true,
            vel: [300.0, 0.0, 0.0], // fast ground movement
            ..Player::default()
        };
        let dt = 1.0 / 72.0;
        p.tick(&map, dt);
        // If ground speed > 10 and on_ground, bob_amount should increase
        let ground_speed = (p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1]).sqrt();
        if ground_speed > 10.0 && p.on_ground {
            assert!(
                p.bob_amount > 0.0,
                "bob_amount should increase with fast ground movement"
            );
        }
    }

    #[test]
    fn tick_view_bob_decays_when_stopped() {
        let map = QuakeMap::new();
        let mut p = Player {
            bob_amount: 1.0,
            on_ground: true,
            vel: [0.0, 0.0, 0.0], // stopped
            ..Player::default()
        };
        let dt = 1.0 / 72.0;
        p.tick(&map, dt);
        assert!(
            p.bob_amount < 1.0,
            "bob_amount should decay when stopped, got {}",
            p.bob_amount
        );
    }

    #[test]
    fn bob_offset_varies_with_phase() {
        let p1 = Player {
            bob_amount: 1.0,
            bob_phase: 0.0,
            ..Player::default()
        };
        let p2 = Player {
            bob_amount: 1.0,
            bob_phase: std::f32::consts::FRAC_PI_4,
            ..Player::default()
        };
        // Different phases should produce different offsets
        assert!(
            (p1.bob_offset() - p2.bob_offset()).abs() > 0.001,
            "different phases should produce different bob offsets"
        );
    }

    #[test]
    fn bob_offset_scales_with_amount() {
        let low = Player {
            bob_amount: 0.1,
            bob_phase: 1.0,
            ..Player::default()
        };
        let high = Player {
            bob_amount: 1.0,
            bob_phase: 1.0,
            ..Player::default()
        };
        assert!(
            high.bob_offset().abs() >= low.bob_offset().abs(),
            "higher bob_amount should produce larger offset: low={}, high={}",
            low.bob_offset(),
            high.bob_offset()
        );
    }

    #[test]
    fn cross_product_orthogonality() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0];
        let c = cross(a, b);
        // cross(x, y) = z
        assert!((c[0]).abs() < 1e-6);
        assert!((c[1]).abs() < 1e-6);
        assert!((c[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cross_product_anticommutative() {
        let a = [1.0f32, 2.0, 3.0];
        let b = [4.0f32, 5.0, 6.0];
        let ab = cross(a, b);
        let ba = cross(b, a);
        for i in 0..3 {
            assert!(
                (ab[i] + ba[i]).abs() < 1e-6,
                "cross product should be anticommutative at component {i}"
            );
        }
    }

    #[test]
    fn strafe_perpendicular_to_forward_movement() {
        let mut p1 = Player::default();
        let mut p2 = Player::default();
        p1.move_forward(1.0);
        p2.strafe(1.0);
        // Velocity vectors should be roughly perpendicular
        let dot = p1.vel[0] * p2.vel[0] + p1.vel[1] * p2.vel[1];
        assert!(
            dot.abs() < 0.01,
            "forward and strafe should be perpendicular, dot={dot}"
        );
    }

    #[test]
    fn running_increases_strafe_speed() {
        let mut p1 = Player::default();
        let mut p2 = Player {
            running: true,
            ..Player::default()
        };
        p1.strafe(1.0);
        p2.strafe(1.0);
        let speed1 = p1.vel[0] * p1.vel[0] + p1.vel[1] * p1.vel[1];
        let speed2 = p2.vel[0] * p2.vel[0] + p2.vel[1] * p2.vel[1];
        assert!(speed2 > speed1, "running should increase strafe speed");
    }

    #[test]
    fn look_negative_yaw_wraps_positive() {
        let mut p = Player::default();
        p.look(-0.5, 0.0);
        assert!(p.yaw >= 0.0 && p.yaw < std::f32::consts::TAU);
    }

    #[test]
    fn move_forward_backward_cancels() {
        let mut p = Player::default();
        p.move_forward(1.0);
        p.move_forward(-1.0);
        let speed = (p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1]).sqrt();
        assert!(
            speed < 0.01,
            "forward+backward should cancel, speed={speed}"
        );
    }

    #[test]
    fn tick_ceiling_clamp() {
        // Create a map with a low ceiling
        let mut map = QuakeMap::new();
        use crate::quake::map::Room;
        map.rooms.push(Room {
            x: -1000.0,
            y: -1000.0,
            width: 2000.0,
            height: 2000.0,
            floor_z: 0.0,
            ceil_z: 50.0, // very low ceiling
            light: 200.0,
        });
        let mut p = Player {
            pos: [0.0, 0.0, 0.0],
            vel: [0.0, 0.0, 1000.0], // huge upward velocity
            on_ground: false,
            noclip: false,
            ..Player::default()
        };
        p.tick(&map, 1.0 / 72.0);
        // Player pos[2] + PLAYER_HEIGHT should not exceed ceil_z
        assert!(
            p.pos[2] + PLAYER_HEIGHT <= 50.0 + 0.1,
            "player should be clamped by ceiling: pos_z={}, height={}",
            p.pos[2],
            PLAYER_HEIGHT
        );
    }
}
