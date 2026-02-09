//! Quake engine for FrankenTUI.
//!
//! A pure-Rust 3D renderer implementing Quake 1's core rendering and physics,
//! designed to run as a terminal visual effect. Renders into a framebuffer
//! that is blitted to a Painter for terminal output.
//!
//! # Architecture
//! ```text
//! Procedural Map → Face Sorting → Triangle Rasterizer → Framebuffer → Painter
//! ```
//!
//! Ported from Quake 1 source (id Software GPL):
//! - Physics: sv_move.c, sv_phys.c
//! - BSP types: bspfile.h
//! - Constants: quakedef.h, sv_phys.c
//! - Rendering: r_main.c, r_edge.c (adapted for face-based rasterization)

#![forbid(unsafe_code)]

pub mod bsp_types;
pub mod constants;
pub mod framebuffer;
pub mod map;
pub mod player;
pub mod renderer;

use ftui_render::cell::PackedRgba;

use crate::canvas::Painter;

use self::constants::*;
use self::framebuffer::QuakeFramebuffer;
use self::map::QuakeMap;
use self::player::Player;
use self::renderer::{QuakeRenderer, RenderStats};

/// The main Quake engine, encapsulating all state.
#[derive(Debug)]
pub struct QuakeEngine {
    /// Parsed map data.
    map: Option<QuakeMap>,
    /// Player state.
    pub player: Player,
    /// Renderer state.
    renderer: QuakeRenderer,
    /// Framebuffer for intermediate rendering.
    framebuffer: QuakeFramebuffer,
    /// Game clock accumulator for fixed-rate game ticks.
    tick_accumulator: f64,
    /// Frame counter.
    pub frame: u64,
    /// Total elapsed time.
    pub time: f64,
    /// Muzzle flash intensity (0.0-1.0).
    pub fire_flash: f32,
    /// Show minimap overlay.
    pub show_minimap: bool,
    /// Show crosshair.
    pub show_crosshair: bool,
    /// Cached render stats from last frame.
    pub last_stats: RenderStats,
    /// Framebuffer resolution width.
    fb_width: u32,
    /// Framebuffer resolution height.
    fb_height: u32,
}

impl QuakeEngine {
    /// Create a new Quake engine (no map loaded).
    pub fn new() -> Self {
        let fb_width = SCREENWIDTH;
        let fb_height = SCREENHEIGHT;

        Self {
            map: None,
            player: Player::default(),
            renderer: QuakeRenderer::new(fb_width, fb_height),
            framebuffer: QuakeFramebuffer::new(fb_width, fb_height),
            tick_accumulator: 0.0,
            frame: 0,
            time: 0.0,
            fire_flash: 0.0,
            show_minimap: false,
            show_crosshair: true,
            last_stats: RenderStats::default(),
            fb_width,
            fb_height,
        }
    }

    /// Load the procedural E1M1-style test map.
    pub fn load_test_map(&mut self) {
        let map = map::generate_e1m1();
        let (px, py, pz, pyaw) = map.player_start();
        self.player.spawn(px, py, pz, pyaw);
        self.map = Some(map);
    }

    /// Update the engine with the given delta time in seconds.
    pub fn update(&mut self, dt: f64) {
        self.time += dt;

        // Accumulate time for fixed-rate game ticks (Quake runs at 72 Hz).
        // Cap at 10 ticks per frame to prevent lag spikes from causing
        // hundreds of physics updates (which could teleport through walls).
        self.tick_accumulator += dt;
        let mut ticks = 0u32;
        while self.tick_accumulator >= TICK_SECS && ticks < 10 {
            self.tick_accumulator -= TICK_SECS;
            self.game_tick();
            ticks += 1;
        }
        if ticks >= 10 {
            self.tick_accumulator = 0.0;
        }

        // Decay muzzle flash
        if self.fire_flash > 0.0 {
            self.fire_flash = (self.fire_flash - dt as f32 * 8.0).max(0.0);
        }
    }

    /// Run one game tick (72 Hz).
    fn game_tick(&mut self) {
        // Split borrow: take map out to avoid &self + &mut self.player conflict
        if let Some(map) = self.map.take() {
            self.player.tick(&map, TICK_SECS as f32);
            self.map = Some(map);
        }
    }

    /// Render the current frame to a Painter.
    pub fn render(&mut self, painter: &mut Painter, _pw: u16, _ph: u16, stride: usize) {
        // Ensure framebuffer matches desired resolution
        if self.framebuffer.width != self.fb_width || self.framebuffer.height != self.fb_height {
            self.framebuffer.resize(self.fb_width, self.fb_height);
            self.renderer.resize(self.fb_width, self.fb_height);
        }

        // Render the scene
        if let Some(map) = self.map.take() {
            self.renderer
                .render(&mut self.framebuffer, &map, &self.player);
            self.last_stats = self.renderer.stats.clone();
            self.map = Some(map);
        } else {
            // No map loaded: clear to fog color
            self.framebuffer.clear();
        }

        // Draw overlays on framebuffer
        if self.show_crosshair {
            self.draw_crosshair();
        }
        if self.fire_flash > 0.0 {
            self.draw_muzzle_flash();
        }
        if self.show_minimap {
            self.draw_minimap();
        }

        // Blit framebuffer to painter
        self.framebuffer.blit_to_painter(painter, stride);
        self.frame += 1;
    }

    // -------------------------------------------------------------------------
    // Player controls (matching Doom engine API)
    // -------------------------------------------------------------------------

    /// Move forward/backward (-1.0 to 1.0).
    pub fn move_forward(&mut self, amount: f32) {
        self.player.move_forward(amount);
    }

    /// Strafe left/right (-1.0 to 1.0).
    pub fn strafe(&mut self, amount: f32) {
        self.player.strafe(amount);
    }

    /// Look (yaw and pitch deltas).
    pub fn look(&mut self, yaw: f32, pitch: f32) {
        self.player.look(yaw, pitch);
    }

    /// Fire weapon (muzzle flash).
    pub fn fire(&mut self) {
        self.fire_flash = 1.0;
    }

    /// Jump.
    pub fn jump(&mut self) {
        self.player.jump();
    }

    /// Toggle noclip mode.
    pub fn toggle_noclip(&mut self) {
        self.player.noclip = !self.player.noclip;
    }

    /// Toggle run mode.
    pub fn toggle_run(&mut self) {
        self.player.running = !self.player.running;
    }

    // -------------------------------------------------------------------------
    // Overlay rendering
    // -------------------------------------------------------------------------

    /// Draw crosshair at screen center.
    fn draw_crosshair(&mut self) {
        let cx = self.framebuffer.width / 2;
        let cy = self.framebuffer.height / 2;
        let color = PackedRgba::rgb(255, 255, 255);
        let size = 3;

        for i in 1..=size {
            self.framebuffer.set_pixel(cx + i, cy, color);
            self.framebuffer.set_pixel(cx.saturating_sub(i), cy, color);
            self.framebuffer.set_pixel(cx, cy + i, color);
            self.framebuffer.set_pixel(cx, cy.saturating_sub(i), color);
        }
    }

    /// Draw muzzle flash overlay.
    fn draw_muzzle_flash(&mut self) {
        let w = self.framebuffer.width;
        let h = self.framebuffer.height;
        let intensity = self.fire_flash;

        // Flash at bottom center (Quake-style yellow/orange flash)
        let cx = w / 2;
        let cy = h - h / 6;
        let radius = (w / 8) as f32 * intensity;

        for y in (cy.saturating_sub(radius as u32))..h.min(cy + radius as u32) {
            for x in (cx.saturating_sub(radius as u32))..w.min(cx + radius as u32) {
                let dx = x as f32 - cx as f32;
                let dy = y as f32 - cy as f32;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < radius {
                    let falloff = 1.0 - dist / radius;
                    let flash = falloff * intensity;
                    let existing = self.framebuffer.get_pixel(x, y);
                    let r = (existing.r() as f32 + 255.0 * flash).min(255.0) as u8;
                    let g = (existing.g() as f32 + 180.0 * flash).min(255.0) as u8;
                    let b = (existing.b() as f32 + 60.0 * flash).min(255.0) as u8;
                    self.framebuffer.set_pixel(x, y, PackedRgba::rgb(r, g, b));
                }
            }
        }
    }

    /// Draw a minimap overlay in the top-right corner.
    fn draw_minimap(&mut self) {
        let map = match &self.map {
            Some(m) => m,
            None => return,
        };

        let map_size = 80u32;
        let margin = 4u32;
        let ox = self.framebuffer.width.saturating_sub(map_size + margin);
        let oy = margin;

        // Draw background
        for y in oy..oy + map_size {
            for x in ox..ox + map_size {
                self.framebuffer
                    .set_pixel(x, y, PackedRgba::rgba(0, 0, 0, 180));
            }
        }

        if map.rooms.is_empty() {
            return;
        }

        // Find map bounds from rooms
        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        for room in &map.rooms {
            min_x = min_x.min(room.x);
            min_y = min_y.min(room.y);
            max_x = max_x.max(room.x + room.width);
            max_y = max_y.max(room.y + room.height);
        }
        let range_x = (max_x - min_x).max(1.0);
        let range_y = (max_y - min_y).max(1.0);
        let scale = (map_size as f32 - 4.0) / range_x.max(range_y);

        // Draw rooms as rectangles
        let room_color = PackedRgba::rgb(0, 120, 0);
        for room in &map.rooms {
            let rx = ox + 2 + ((room.x - min_x) * scale) as u32;
            let ry = oy + 2 + ((room.y - min_y) * scale) as u32;
            let rw = (room.width * scale).max(1.0) as u32;
            let rh = (room.height * scale).max(1.0) as u32;

            // Draw room outline (saturating_add prevents u32 overflow)
            for x in rx..rx.saturating_add(rw) {
                self.framebuffer.set_pixel(x, ry, room_color);
                self.framebuffer
                    .set_pixel(x, ry.saturating_add(rh), room_color);
            }
            for y in ry..ry.saturating_add(rh) {
                self.framebuffer.set_pixel(rx, y, room_color);
                self.framebuffer
                    .set_pixel(rx.saturating_add(rw), y, room_color);
            }
        }

        // Draw player position (clamp to minimap bounds)
        let px = ox + 2 + ((self.player.pos[0] - min_x) * scale).max(0.0) as u32;
        let py = oy + 2 + ((self.player.pos[1] - min_y) * scale).max(0.0) as u32;
        let player_color = PackedRgba::rgb(255, 255, 0);
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                self.framebuffer.set_pixel(
                    (px as i32 + dx).max(0) as u32,
                    (py as i32 + dy).max(0) as u32,
                    player_color,
                );
            }
        }

        // Draw player direction
        let dir_len = 6.0;
        let dir_x = (px as f32 + self.player.yaw.cos() * dir_len).max(0.0);
        let dir_y = (py as f32 + self.player.yaw.sin() * dir_len).max(0.0);
        draw_line_fb(
            &mut self.framebuffer,
            px,
            py,
            dir_x as u32,
            dir_y as u32,
            player_color,
        );
    }
}

impl Default for QuakeEngine {
    fn default() -> Self {
        let mut engine = Self::new();
        engine.load_test_map();
        engine
    }
}

/// Draw a line on the framebuffer using Bresenham's algorithm.
fn draw_line_fb(fb: &mut QuakeFramebuffer, x0: u32, y0: u32, x1: u32, y1: u32, color: PackedRgba) {
    let mut x0 = x0 as i32;
    let mut y0 = y0 as i32;
    let x1 = x1 as i32;
    let y1 = y1 as i32;

    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        if x0 >= 0 && y0 >= 0 {
            fb.set_pixel(x0 as u32, y0 as u32, color);
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_default_creates_test_map() {
        let engine = QuakeEngine::default();
        assert!(engine.map.is_some());
    }

    #[test]
    fn engine_update_advances_time() {
        let mut engine = QuakeEngine::default();
        engine.update(0.1);
        assert!(engine.time > 0.0);
    }

    #[test]
    fn engine_fire_sets_flash() {
        let mut engine = QuakeEngine::default();
        engine.fire();
        assert!((engine.fire_flash - 1.0).abs() < 0.01);
    }

    #[test]
    fn engine_toggles() {
        let mut engine = QuakeEngine::default();
        assert!(!engine.player.noclip);
        engine.toggle_noclip();
        assert!(engine.player.noclip);
        engine.toggle_noclip();
        assert!(!engine.player.noclip);
    }

    #[test]
    fn engine_movement() {
        let mut engine = QuakeEngine::default();
        let start_pos = engine.player.pos;
        engine.move_forward(1.0);
        engine.update(0.1);
        // Position should have changed due to velocity + physics tick
        let moved = (engine.player.pos[0] - start_pos[0]).abs()
            + (engine.player.pos[1] - start_pos[1]).abs();
        assert!(moved > 0.0);
    }

    #[test]
    fn render_to_framebuffer() {
        let mut engine = QuakeEngine::default();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        assert!(engine.frame > 0);
    }

    #[test]
    fn engine_jump() {
        let mut engine = QuakeEngine::default();
        assert!(engine.player.on_ground);
        engine.jump();
        assert!(!engine.player.on_ground);
    }

    #[test]
    fn engine_new_no_map() {
        let engine = QuakeEngine::new();
        assert!(engine.map.is_none());
        assert_eq!(engine.frame, 0);
        assert_eq!(engine.time, 0.0);
    }

    #[test]
    fn engine_load_test_map() {
        let mut engine = QuakeEngine::new();
        engine.load_test_map();
        assert!(engine.map.is_some());
    }

    #[test]
    fn engine_fire_flash_decays() {
        let mut engine = QuakeEngine::default();
        engine.fire();
        assert!((engine.fire_flash - 1.0).abs() < 0.01);
        engine.update(0.5);
        assert!(engine.fire_flash < 1.0, "flash should decay after update");
    }

    #[test]
    fn engine_update_caps_ticks() {
        let mut engine = QuakeEngine::default();
        // Very large dt should be capped (10 ticks max)
        engine.update(10.0);
        // Engine should still be in a valid state
        assert!(engine.time > 0.0);
    }

    #[test]
    fn engine_toggle_run() {
        let mut engine = QuakeEngine::default();
        assert!(!engine.player.running);
        engine.toggle_run();
        assert!(engine.player.running);
        engine.toggle_run();
        assert!(!engine.player.running);
    }

    #[test]
    fn engine_strafe_changes_velocity() {
        let mut engine = QuakeEngine::default();
        engine.strafe(1.0);
        let vel_mag = engine.player.vel[0].abs() + engine.player.vel[1].abs();
        assert!(vel_mag > 0.0, "strafing should add velocity");
    }

    #[test]
    fn engine_look_changes_yaw() {
        let mut engine = QuakeEngine::default();
        let original_yaw = engine.player.yaw;
        engine.look(0.5, 0.0);
        assert!((engine.player.yaw - original_yaw).abs() > 0.01);
    }

    #[test]
    fn engine_show_crosshair_default_true() {
        let engine = QuakeEngine::default();
        assert!(engine.show_crosshair);
        assert!(!engine.show_minimap);
    }

    #[test]
    fn engine_render_no_map() {
        let mut engine = QuakeEngine::new();
        let mut painter = Painter::new(120, 80, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 60, 20, 1);
        // Should not panic even without a map
        assert_eq!(engine.frame, 1);
    }

    #[test]
    fn engine_render_with_minimap() {
        let mut engine = QuakeEngine {
            show_minimap: true,
            ..QuakeEngine::default()
        };
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        // Should not panic with minimap enabled
    }

    #[test]
    fn engine_render_with_fire_flash() {
        let mut engine = QuakeEngine::default();
        engine.fire();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        // Should not panic with fire flash active
    }

    #[test]
    fn draw_line_fb_horizontal_is_inclusive() {
        let mut fb = QuakeFramebuffer::new(8, 6);
        let color = PackedRgba::GREEN;
        draw_line_fb(&mut fb, 1, 3, 5, 3, color);

        for x in 1..=5 {
            assert_eq!(fb.get_pixel(x, 3), color);
        }
        assert_eq!(fb.get_pixel(0, 3), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(6, 3), PackedRgba::BLACK);
    }

    #[test]
    fn draw_line_fb_reverse_diagonal_draws_expected_pixels() {
        let mut fb = QuakeFramebuffer::new(7, 7);
        let color = PackedRgba::RED;
        draw_line_fb(&mut fb, 5, 5, 1, 1, color);

        for i in 1..=5 {
            assert_eq!(fb.get_pixel(i, i), color);
        }
        assert_eq!(fb.get_pixel(0, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(6, 6), PackedRgba::BLACK);
    }

    #[test]
    fn draw_line_fb_clips_via_framebuffer_bounds() {
        let mut fb = QuakeFramebuffer::new(5, 3);
        let color = PackedRgba::BLUE;
        draw_line_fb(&mut fb, 0, 1, 10, 1, color);

        for x in 0..5 {
            assert_eq!(fb.get_pixel(x, 1), color);
        }
    }

    #[test]
    fn draw_crosshair_sets_arm_pixels() {
        let mut engine = QuakeEngine::new();
        engine.framebuffer.clear();
        engine.draw_crosshair();

        let cx = engine.framebuffer.width / 2;
        let cy = engine.framebuffer.height / 2;
        for i in 1..=3 {
            assert_eq!(engine.framebuffer.get_pixel(cx + i, cy), PackedRgba::WHITE);
            assert_eq!(engine.framebuffer.get_pixel(cx - i, cy), PackedRgba::WHITE);
            assert_eq!(engine.framebuffer.get_pixel(cx, cy + i), PackedRgba::WHITE);
            assert_eq!(engine.framebuffer.get_pixel(cx, cy - i), PackedRgba::WHITE);
        }
        assert_eq!(engine.framebuffer.get_pixel(cx, cy), PackedRgba::BLACK);
    }

    #[test]
    fn draw_muzzle_flash_tints_bottom_center() {
        let mut engine = QuakeEngine::new();
        engine.framebuffer.clear();
        engine.fire_flash = 1.0;
        engine.draw_muzzle_flash();

        let cx = engine.framebuffer.width / 2;
        let cy = engine.framebuffer.height - engine.framebuffer.height / 6;
        let flash = engine.framebuffer.get_pixel(cx, cy);
        assert!(flash.r() > 0 && flash.g() > 0 && flash.b() > 0);
        assert!(flash.r() >= flash.g() && flash.g() >= flash.b());
        assert_eq!(engine.framebuffer.get_pixel(0, 0), PackedRgba::BLACK);
    }

    #[test]
    fn draw_minimap_without_map_is_noop() {
        let mut engine = QuakeEngine::new();
        engine.framebuffer.clear();
        let before = engine.framebuffer.pixels.clone();

        engine.draw_minimap();

        assert_eq!(engine.framebuffer.pixels, before);
    }

    #[test]
    fn draw_minimap_with_map_draws_overlay_background() {
        let mut engine = QuakeEngine::default();
        engine.framebuffer.clear();
        engine.draw_minimap();

        let map_size = 80u32;
        let margin = 4u32;
        let ox = engine.framebuffer.width.saturating_sub(map_size + margin);
        let oy = margin;
        let bg = engine.framebuffer.get_pixel(ox + 1, oy + 1);
        assert_eq!(bg.a(), 180);
    }

    #[test]
    fn engine_lifecycle_load_render_update_keeps_map_consistent() {
        let mut engine = QuakeEngine::new();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);

        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 1);
        assert!(engine.map.is_none());

        engine.load_test_map();
        assert!(engine.map.is_some());

        let time_before = engine.time;
        engine.update(TICK_SECS * 3.5);
        assert!(engine.time > time_before);
        assert!(engine.tick_accumulator < TICK_SECS);

        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 2);
        assert!(engine.map.is_some());
    }

    #[test]
    fn engine_render_resizes_framebuffer_to_configured_dimensions() {
        let mut engine = QuakeEngine::new();
        engine.fb_width = 64;
        engine.fb_height = 40;

        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);

        assert_eq!(engine.framebuffer.width, 64);
        assert_eq!(engine.framebuffer.height, 40);
    }

    // ---- draw_line_fb: additional Bresenham edge cases ----

    #[test]
    fn draw_line_fb_vertical_is_inclusive() {
        let mut fb = QuakeFramebuffer::new(6, 10);
        let color = PackedRgba::rgb(0, 255, 0);
        draw_line_fb(&mut fb, 3, 1, 3, 7, color);
        for y in 1..=7 {
            assert_eq!(fb.get_pixel(3, y), color, "pixel at (3, {y}) should be set");
        }
        assert_eq!(fb.get_pixel(3, 0), PackedRgba::BLACK);
        assert_eq!(fb.get_pixel(3, 8), PackedRgba::BLACK);
    }

    #[test]
    fn draw_line_fb_single_point() {
        let mut fb = QuakeFramebuffer::new(10, 10);
        let color = PackedRgba::rgb(0, 0, 255);
        draw_line_fb(&mut fb, 4, 4, 4, 4, color);
        assert_eq!(fb.get_pixel(4, 4), color);
    }

    #[test]
    fn draw_line_fb_steep_slope() {
        let mut fb = QuakeFramebuffer::new(10, 20);
        let color = PackedRgba::rgb(200, 100, 50);
        // Steep: 1 pixel wide, 8 pixels tall
        draw_line_fb(&mut fb, 3, 1, 4, 9, color);
        assert_eq!(fb.get_pixel(3, 1), color);
        assert_eq!(fb.get_pixel(4, 9), color);
    }

    #[test]
    fn draw_line_fb_reverse_vertical() {
        let mut fb = QuakeFramebuffer::new(6, 10);
        let color = PackedRgba::rgb(128, 0, 128);
        draw_line_fb(&mut fb, 2, 7, 2, 2, color);
        for y in 2..=7 {
            assert_eq!(
                fb.get_pixel(2, y),
                color,
                "reverse vertical: pixel at (2, {y}) should be set"
            );
        }
    }

    #[test]
    fn draw_line_fb_at_origin() {
        let mut fb = QuakeFramebuffer::new(5, 5);
        let color = PackedRgba::rgb(255, 0, 255);
        draw_line_fb(&mut fb, 0, 0, 0, 0, color);
        assert_eq!(fb.get_pixel(0, 0), color);
    }

    #[test]
    fn draw_line_fb_gentle_slope() {
        let mut fb = QuakeFramebuffer::new(20, 10);
        let color = PackedRgba::rgb(100, 200, 100);
        // Gentle: 8 pixels wide, 2 pixels tall
        draw_line_fb(&mut fb, 2, 3, 10, 5, color);
        assert_eq!(fb.get_pixel(2, 3), color);
        assert_eq!(fb.get_pixel(10, 5), color);
        // Intermediate pixels should be set (at least some in between)
        let mut count = 0u32;
        for x in 2..=10 {
            for y in 3..=5 {
                if fb.get_pixel(x, y) == color {
                    count += 1;
                }
            }
        }
        // Bresenham should set at least as many pixels as the longer dimension
        assert!(
            count >= 9,
            "gentle slope should set at least 9 pixels, got {count}"
        );
    }

    // ---- Overlay: additional edge cases ----

    #[test]
    fn draw_muzzle_flash_zero_intensity_is_noop() {
        let mut engine = QuakeEngine::new();
        engine.framebuffer.clear();
        engine.fire_flash = 0.0;
        let before = engine.framebuffer.pixels.clone();
        // fire_flash = 0.0 means draw_muzzle_flash won't be called by render,
        // but we can call it directly — radius will be 0
        engine.draw_muzzle_flash();
        assert_eq!(engine.framebuffer.pixels, before);
    }

    #[test]
    fn draw_crosshair_center_pixel_untouched() {
        // Crosshair draws arms from i=1..=3, center pixel (cx,cy) should be unchanged
        let mut engine = QuakeEngine::new();
        engine.framebuffer.clear();
        let cx = engine.framebuffer.width / 2;
        let cy = engine.framebuffer.height / 2;
        let before_center = engine.framebuffer.get_pixel(cx, cy);
        engine.draw_crosshair();
        assert_eq!(engine.framebuffer.get_pixel(cx, cy), before_center);
    }

    // ---- Engine lifecycle: additional edge cases ----

    #[test]
    fn lifecycle_multiple_renders_increment_frame() {
        let mut engine = QuakeEngine::default();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        for i in 1..=5 {
            engine.render(&mut painter, 120, 40, 1);
            assert_eq!(engine.frame, i);
        }
    }

    #[test]
    fn lifecycle_render_without_update() {
        let mut engine = QuakeEngine::default();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 1);
        assert_eq!(engine.time, 0.0);
    }

    #[test]
    fn lifecycle_update_without_render() {
        let mut engine = QuakeEngine::default();
        engine.move_forward(1.0);
        engine.strafe(-0.5);
        engine.look(0.1, 0.0);
        for _ in 0..10 {
            engine.update(0.016);
        }
        assert!(engine.time > 0.1);
        assert_eq!(engine.frame, 0);
    }

    #[test]
    fn lifecycle_zero_dt_update() {
        let mut engine = QuakeEngine::default();
        engine.update(0.0);
        assert_eq!(engine.time, 0.0);
    }

    #[test]
    fn lifecycle_very_small_dt_no_tick() {
        let mut engine = QuakeEngine::default();
        let start_pos = engine.player.pos;
        // dt smaller than one tick (1/72 ≈ 0.0139s) should not trigger game_tick
        engine.update(0.001);
        assert!((engine.time - 0.001).abs() < f64::EPSILON);
        assert_eq!(engine.player.pos, start_pos);
    }

    #[test]
    fn lifecycle_fire_flash_fully_decays() {
        let mut engine = QuakeEngine::default();
        engine.fire();
        // Flash decays at rate 8.0/s, starting from 1.0 → zero after 0.125s
        engine.update(0.2);
        assert_eq!(engine.fire_flash, 0.0, "flash should be fully decayed");
    }

    #[test]
    fn lifecycle_all_controls_in_sequence() {
        let mut engine = QuakeEngine::default();
        engine.move_forward(1.0);
        engine.strafe(0.5);
        engine.look(0.2, -0.1);
        engine.fire();
        engine.jump();
        engine.toggle_noclip();
        engine.toggle_run();
        engine.update(0.016);

        assert!(!engine.player.on_ground);
        assert!(engine.player.noclip);
        assert!(engine.player.running);
        assert!(engine.fire_flash > 0.0);
    }

    #[test]
    fn lifecycle_default_spawns_player_at_map_start() {
        let engine = QuakeEngine::default();
        assert!(engine.map.is_some());
        let pos = engine.player.pos;
        let dist = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
        assert!(dist > 0.0, "player should be spawned away from origin");
    }

    #[test]
    fn lifecycle_render_various_strides() {
        let mut engine = QuakeEngine::default();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 1);
        engine.render(&mut painter, 120, 40, 2);
        assert_eq!(engine.frame, 2);
        engine.render(&mut painter, 120, 40, 4);
        assert_eq!(engine.frame, 3);
    }
}
