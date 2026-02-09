//! Doom engine for FrankenTUI.
//!
//! A pure-Rust BSP renderer implementing the core Doom rendering algorithm,
//! designed to run as a terminal visual effect. Renders into a framebuffer
//! that is blitted to a Painter for terminal output.
//!
//! # Architecture
//! ```text
//! WAD file → DoomMap → BSP Traversal → Column Renderer → Framebuffer → Painter
//! ```
//!
//! # Phase 1: BSP Renderer + Movement
//! - WAD parsing and map loading
//! - BSP front-to-back wall rendering (solid colors)
//! - Perspective-correct wall projection
//! - Distance-based lighting (COLORMAP fog)
//! - Player movement with collision detection
//! - Sky/floor gradients

#![forbid(unsafe_code)]

pub mod bsp;
pub mod constants;
pub mod framebuffer;
pub mod geometry;
pub mod map;
pub mod palette;
pub mod player;
pub mod renderer;
pub mod tables;
pub mod wad;
pub mod wad_types;

use ftui_render::cell::PackedRgba;

use crate::canvas::Painter;

use self::constants::*;
use self::framebuffer::DoomFramebuffer;
use self::map::DoomMap;
use self::palette::DoomPalette;
use self::player::Player;
use self::renderer::{DoomRenderer, RenderStats};
use self::wad::WadFile;

/// The main Doom engine, encapsulating all state.
#[derive(Debug)]
pub struct DoomEngine {
    /// Parsed map data.
    map: Option<DoomMap>,
    /// Player state.
    pub player: Player,
    /// Palette and colormaps.
    palette: DoomPalette,
    /// Renderer state.
    renderer: DoomRenderer,
    /// Framebuffer for intermediate rendering.
    framebuffer: DoomFramebuffer,
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
    /// Show performance overlay.
    pub show_perf: bool,
    /// Show crosshair.
    pub show_crosshair: bool,
    /// Use original palette colors.
    pub original_palette: bool,
    /// Frame cap enabled.
    pub vsync: bool,
    /// Show HUD info.
    pub show_hud: bool,
    /// Cached render stats from last frame.
    pub last_stats: RenderStats,
    /// Framebuffer resolution width.
    fb_width: u32,
    /// Framebuffer resolution height.
    fb_height: u32,
}

impl DoomEngine {
    /// Create a new Doom engine (no map loaded).
    pub fn new() -> Self {
        let fb_width = SCREENWIDTH;
        let fb_height = SCREENHEIGHT;

        Self {
            map: None,
            player: Player::default(),
            palette: DoomPalette::default(),
            renderer: DoomRenderer::new(fb_width, fb_height),
            framebuffer: DoomFramebuffer::new(fb_width, fb_height),
            tick_accumulator: 0.0,
            frame: 0,
            time: 0.0,
            fire_flash: 0.0,
            show_minimap: false,
            show_perf: false,
            show_crosshair: true,
            original_palette: false,
            vsync: true,
            show_hud: true,
            last_stats: RenderStats::default(),
            fb_width,
            fb_height,
        }
    }

    /// Load a map from WAD data.
    pub fn load_wad(&mut self, wad_data: Vec<u8>, map_name: &str) -> Result<(), String> {
        let wad = WadFile::parse(wad_data).map_err(|e| e.to_string())?;

        // Load palette if available
        if let Ok(colors) = wad.parse_playpal() {
            let colormaps = wad.parse_colormap().unwrap_or_default();
            self.palette = DoomPalette::from_wad(colors, colormaps);
        }

        let map = DoomMap::load(&wad, map_name).map_err(|e| e.to_string())?;

        // Find player start
        if let Some((x, y, angle)) = map.player_start() {
            self.player.spawn(x, y, angle);
        }

        self.map = Some(map);
        Ok(())
    }

    /// Load a procedurally generated test map.
    pub fn load_test_map(&mut self) {
        self.map = Some(generate_test_map());
        self.player.spawn(0.0, 0.0, 0.0);
    }

    /// Update the engine with the given delta time in seconds.
    pub fn update(&mut self, dt: f64) {
        self.time += dt;

        // Accumulate time for fixed-rate game ticks
        self.tick_accumulator += dt;
        while self.tick_accumulator >= DOOM_TICK_SECS {
            self.tick_accumulator -= DOOM_TICK_SECS;
            self.game_tick();
        }

        // Decay muzzle flash
        if self.fire_flash > 0.0 {
            self.fire_flash = (self.fire_flash - dt as f32 * 8.0).max(0.0);
        }
    }

    /// Run one game tick (35 Hz).
    fn game_tick(&mut self) {
        // Split borrow: take map out temporarily to avoid &self + &mut self.player conflict
        if let Some(map) = self.map.take() {
            self.player.tick(&map);
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
        // Split borrow: take map out to avoid &self.map + &mut self.renderer conflict
        if let Some(map) = self.map.take() {
            self.renderer
                .render(&mut self.framebuffer, &map, &self.player, &self.palette);
            self.last_stats = self.renderer.stats.clone();
            self.map = Some(map);
        } else {
            // No map loaded: show test pattern
            self.render_no_map();
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

    /// Player movement controls.
    pub fn move_forward(&mut self, amount: f32) {
        self.player.move_forward(amount);
    }

    pub fn strafe(&mut self, amount: f32) {
        self.player.strafe(amount);
    }

    pub fn look(&mut self, yaw: f32, pitch: f32) {
        self.player.look(yaw, pitch);
    }

    pub fn fire(&mut self) {
        self.fire_flash = 1.0;
    }

    pub fn toggle_noclip(&mut self) {
        self.player.noclip = !self.player.noclip;
    }

    pub fn toggle_god_mode(&mut self) {
        self.player.god_mode = !self.player.god_mode;
    }

    pub fn toggle_run(&mut self) {
        self.player.running = !self.player.running;
    }

    /// Render a "no map loaded" test pattern.
    fn render_no_map(&mut self) {
        self.framebuffer.clear();
        let w = self.framebuffer.width;
        let h = self.framebuffer.height;

        // Draw a simple raycaster-like test scene
        let cos_a = self.player.angle.cos();
        let sin_a = self.player.angle.sin();

        for x in 0..w {
            // Cast a ray for this column
            let screen_x = (x as f32 / w as f32) * 2.0 - 1.0;
            let ray_cos = cos_a - screen_x * sin_a * 0.5;
            let ray_sin = sin_a + screen_x * cos_a * 0.5;

            // Simple grid-based raycasting
            let mut dist = 0.0f32;
            let mut hit = false;
            let step = 2.0;

            for _ in 0..100 {
                dist += step;
                let mx = self.player.x + ray_cos * dist;
                let my = self.player.y + ray_sin * dist;

                // Check against a simple room
                let gx = (mx / 128.0).floor() as i32;
                let gy = (my / 128.0).floor() as i32;
                if !(-2..=2).contains(&gx) || !(-2..=2).contains(&gy) {
                    hit = true;
                    break;
                }
            }

            if hit {
                let corrected = dist * (screen_x * 0.5).cos();
                let wall_h = (128.0 / corrected * (h as f32 / 2.0)).min(h as f32);
                let top = ((h as f32 - wall_h) / 2.0) as u32;
                let bottom = ((h as f32 + wall_h) / 2.0) as u32;

                let light = (1.0 / (1.0 + dist / 300.0)).clamp(0.0, 1.0);
                let r = (160.0 * light) as u8;
                let g = (120.0 * light) as u8;
                let b = (80.0 * light) as u8;

                self.framebuffer
                    .draw_column(x, top, bottom, PackedRgba::rgb(r, g, b));

                // Sky
                for y in 0..top {
                    let sky_t = y as f32 / (h as f32 / 2.0);
                    let sr = (80.0 + 80.0 * sky_t) as u8;
                    let sg = (120.0 + 80.0 * sky_t) as u8;
                    let sb = (200.0 + 40.0 * sky_t.min(1.0)) as u8;
                    self.framebuffer
                        .set_pixel(x, y, PackedRgba::rgb(sr, sg, sb));
                }

                // Floor
                for y in bottom..h {
                    let floor_t = (y - bottom) as f32 / (h - bottom).max(1) as f32;
                    let fr = (70.0 + 50.0 * floor_t) as u8;
                    let fg = (60.0 + 40.0 * floor_t) as u8;
                    let fb_c = (50.0 + 30.0 * floor_t) as u8;
                    self.framebuffer
                        .set_pixel(x, y, PackedRgba::rgb(fr, fg, fb_c));
                }
            }
        }
    }

    /// Draw crosshair at screen center.
    fn draw_crosshair(&mut self) {
        let cx = self.framebuffer.width / 2;
        let cy = self.framebuffer.height / 2;
        let color = PackedRgba::rgb(255, 255, 255);
        let size = 3;

        for i in 1..=size {
            self.framebuffer.set_pixel(cx + i, cy, color);
            self.framebuffer.set_pixel(cx - i, cy, color);
            self.framebuffer.set_pixel(cx, cy + i, color);
            self.framebuffer.set_pixel(cx, cy - i, color);
        }
    }

    /// Draw muzzle flash overlay.
    fn draw_muzzle_flash(&mut self) {
        let w = self.framebuffer.width;
        let h = self.framebuffer.height;
        let intensity = self.fire_flash;

        // Flash at bottom center
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
                    let g = (existing.g() as f32 + 200.0 * flash).min(255.0) as u8;
                    let b = (existing.b() as f32 + 100.0 * flash).min(255.0) as u8;
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

        let map_size = 80u32; // Pixels for minimap
        let margin = 4u32;
        let ox = self.framebuffer.width - map_size - margin;
        let oy = margin;

        // Draw background
        for y in oy..oy + map_size {
            for x in ox..ox + map_size {
                self.framebuffer
                    .set_pixel(x, y, PackedRgba::rgba(0, 0, 0, 180));
            }
        }

        if map.vertices.is_empty() {
            return;
        }

        // Find map bounds
        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        for v in &map.vertices {
            min_x = min_x.min(v.x);
            min_y = min_y.min(v.y);
            max_x = max_x.max(v.x);
            max_y = max_y.max(v.y);
        }
        let range_x = (max_x - min_x).max(1.0);
        let range_y = (max_y - min_y).max(1.0);
        let scale = (map_size as f32 - 4.0) / range_x.max(range_y);

        let map_to_screen = |mx: f32, my: f32| -> (u32, u32) {
            let sx = ox + 2 + ((mx - min_x) * scale) as u32;
            let sy = oy + 2 + ((max_y - my) * scale) as u32; // Flip Y
            (sx.min(ox + map_size - 1), sy.min(oy + map_size - 1))
        };

        // Draw linedefs
        let line_color = PackedRgba::rgb(0, 180, 0);
        for linedef in &map.linedefs {
            if linedef.v1 >= map.vertices.len() || linedef.v2 >= map.vertices.len() {
                continue;
            }
            let v1 = &map.vertices[linedef.v1];
            let v2 = &map.vertices[linedef.v2];
            let (sx1, sy1) = map_to_screen(v1.x, v1.y);
            let (sx2, sy2) = map_to_screen(v2.x, v2.y);
            draw_line_fb(&mut self.framebuffer, sx1, sy1, sx2, sy2, line_color);
        }

        // Draw player position
        let (px, py) = map_to_screen(self.player.x, self.player.y);
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
        let dir_x = px as f32 + self.player.angle.cos() * dir_len;
        let dir_y = py as f32 - self.player.angle.sin() * dir_len;
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

impl Default for DoomEngine {
    fn default() -> Self {
        let mut engine = Self::new();
        engine.load_test_map();
        engine
    }
}

/// Draw a line on the framebuffer using Bresenham's algorithm.
fn draw_line_fb(fb: &mut DoomFramebuffer, x0: u32, y0: u32, x1: u32, y1: u32, color: PackedRgba) {
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
        fb.set_pixel(x0 as u32, y0 as u32, color);
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

/// Generate a test map for when no WAD is available.
fn generate_test_map() -> DoomMap {
    use self::map::*;

    // Create a simple multi-room map
    let mut vertices = Vec::new();
    let mut linedefs = Vec::new();
    let mut sidedefs = Vec::new();
    let mut sectors = Vec::new();
    let mut segs = Vec::new();
    let mut subsectors = Vec::new();
    let mut nodes = Vec::new();

    // Room dimensions (keep small so walls are visible from player start)
    let room_size = 256.0f32;
    let corridor_width = 64.0f32;
    let wall_height = 128.0f32;

    // Sector 0: Main room
    sectors.push(Sector {
        floor_height: 0.0,
        ceiling_height: wall_height,
        floor_texture: "FLOOR0_1".into(),
        ceiling_texture: "CEIL1_1".into(),
        light_level: 200,
        special: 0,
        tag: 0,
    });

    // Sector 1: East corridor
    sectors.push(Sector {
        floor_height: 0.0,
        ceiling_height: wall_height - 16.0,
        floor_texture: "FLOOR0_1".into(),
        ceiling_texture: "CEIL1_1".into(),
        light_level: 160,
        special: 0,
        tag: 0,
    });

    // Sector 2: North room (elevated)
    sectors.push(Sector {
        floor_height: 16.0,
        ceiling_height: wall_height + 32.0,
        floor_texture: "FLOOR0_1".into(),
        ceiling_texture: "F_SKY1".into(),
        light_level: 220,
        special: 0,
        tag: 0,
    });

    // Sector 3: South alcove (dark)
    sectors.push(Sector {
        floor_height: -16.0,
        ceiling_height: wall_height - 32.0,
        floor_texture: "FLOOR0_1".into(),
        ceiling_texture: "CEIL1_1".into(),
        light_level: 80,
        special: 0,
        tag: 0,
    });

    // Main room vertices (0-3)
    vertices.push(Vertex {
        x: -room_size,
        y: -room_size,
    }); // 0: SW
    vertices.push(Vertex {
        x: room_size,
        y: -room_size,
    }); // 1: SE
    vertices.push(Vertex {
        x: room_size,
        y: room_size,
    }); // 2: NE
    vertices.push(Vertex {
        x: -room_size,
        y: room_size,
    }); // 3: NW

    // East corridor vertices (4-7)
    vertices.push(Vertex {
        x: room_size,
        y: -corridor_width,
    }); // 4
    vertices.push(Vertex {
        x: room_size * 2.0,
        y: -corridor_width,
    }); // 5
    vertices.push(Vertex {
        x: room_size * 2.0,
        y: corridor_width,
    }); // 6
    vertices.push(Vertex {
        x: room_size,
        y: corridor_width,
    }); // 7

    // North room vertices (8-11)
    vertices.push(Vertex {
        x: -corridor_width,
        y: room_size,
    }); // 8
    vertices.push(Vertex {
        x: corridor_width,
        y: room_size,
    }); // 9
    vertices.push(Vertex {
        x: corridor_width,
        y: room_size * 2.0,
    }); // 10
    vertices.push(Vertex {
        x: -corridor_width,
        y: room_size * 2.0,
    }); // 11

    // South alcove vertices (12-15)
    vertices.push(Vertex {
        x: -corridor_width,
        y: -room_size,
    }); // 12
    vertices.push(Vertex {
        x: corridor_width,
        y: -room_size,
    }); // 13
    vertices.push(Vertex {
        x: corridor_width,
        y: -room_size - 128.0,
    }); // 14
    vertices.push(Vertex {
        x: -corridor_width,
        y: -room_size - 128.0,
    }); // 15

    // Sidedefs for main room walls (sector 0)
    let main_side = sidedefs.len();
    sidedefs.push(SideDef {
        x_offset: 0.0,
        y_offset: 0.0,
        upper_texture: "-".into(),
        lower_texture: "-".into(),
        middle_texture: "STARTAN3".into(),
        sector: 0,
    });

    // Sidedefs for corridor (sector 1)
    let corr_side = sidedefs.len();
    sidedefs.push(SideDef {
        x_offset: 0.0,
        y_offset: 0.0,
        upper_texture: "-".into(),
        lower_texture: "-".into(),
        middle_texture: "STARG3".into(),
        sector: 1,
    });

    // Sidedefs for north room (sector 2)
    let north_side = sidedefs.len();
    sidedefs.push(SideDef {
        x_offset: 0.0,
        y_offset: 0.0,
        upper_texture: "STARTAN3".into(),
        lower_texture: "STARTAN3".into(),
        middle_texture: "-".into(),
        sector: 2,
    });

    // Sidedefs for south alcove (sector 3)
    let south_side = sidedefs.len();
    sidedefs.push(SideDef {
        x_offset: 0.0,
        y_offset: 0.0,
        upper_texture: "-".into(),
        lower_texture: "-".into(),
        middle_texture: "STARG3".into(),
        sector: 3,
    });

    // Main room linedefs (south, east, north, west walls)
    // South wall
    linedefs.push(LineDef {
        v1: 0,
        v2: 1,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });
    // East wall (with opening for corridor)
    linedefs.push(LineDef {
        v1: 1,
        v2: 4,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });
    // East opening to corridor (two-sided)
    linedefs.push(LineDef {
        v1: 4,
        v2: 7,
        flags: wad_types::ML_TWOSIDED,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: Some(corr_side),
    });
    linedefs.push(LineDef {
        v1: 7,
        v2: 2,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });
    // North wall (with opening for north room)
    linedefs.push(LineDef {
        v1: 2,
        v2: 9,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 9,
        v2: 8,
        flags: wad_types::ML_TWOSIDED,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: Some(north_side),
    });
    linedefs.push(LineDef {
        v1: 8,
        v2: 3,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });
    // West wall
    linedefs.push(LineDef {
        v1: 3,
        v2: 0,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: None,
    });

    // East corridor walls
    linedefs.push(LineDef {
        v1: 4,
        v2: 5,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(corr_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 5,
        v2: 6,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(corr_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 6,
        v2: 7,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(corr_side),
        back_sidedef: None,
    });

    // North room walls
    linedefs.push(LineDef {
        v1: 9,
        v2: 10,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(north_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 10,
        v2: 11,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(north_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 11,
        v2: 8,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(north_side),
        back_sidedef: None,
    });

    // South alcove walls and opening
    linedefs.push(LineDef {
        v1: 12,
        v2: 13,
        flags: wad_types::ML_TWOSIDED,
        special: 0,
        tag: 0,
        front_sidedef: Some(main_side),
        back_sidedef: Some(south_side),
    });
    linedefs.push(LineDef {
        v1: 13,
        v2: 14,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(south_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 14,
        v2: 15,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(south_side),
        back_sidedef: None,
    });
    linedefs.push(LineDef {
        v1: 15,
        v2: 12,
        flags: wad_types::ML_BLOCKING,
        special: 0,
        tag: 0,
        front_sidedef: Some(south_side),
        back_sidedef: None,
    });

    // Generate BSP data from linedefs (simplified: one subsector per linedef group)
    // For a proper BSP we'd need a node builder, but for a test map we create
    // a simple flat structure

    // Create segs from linedefs
    for (i, linedef) in linedefs.iter().enumerate() {
        segs.push(Seg {
            v1: linedef.v1,
            v2: linedef.v2,
            angle: geometry::point_to_angle(
                vertices[linedef.v1].x,
                vertices[linedef.v1].y,
                vertices[linedef.v2].x,
                vertices[linedef.v2].y,
            ),
            linedef: i,
            direction: 0,
            offset: 0.0,
        });
    }

    // Create subsectors: group segs by sector
    // Main room segs: 0-7
    subsectors.push(SubSector {
        num_segs: 8,
        first_seg: 0,
    });
    // Corridor segs: 8-10
    subsectors.push(SubSector {
        num_segs: 3,
        first_seg: 8,
    });
    // North room segs: 11-13
    subsectors.push(SubSector {
        num_segs: 3,
        first_seg: 11,
    });
    // South alcove segs: 14-17
    subsectors.push(SubSector {
        num_segs: 4,
        first_seg: 14,
    });

    // Create a simple BSP tree (binary split)
    // Node 0: split between main room and east side
    nodes.push(Node {
        x: room_size,
        y: 0.0,
        dx: 0.0,
        dy: 1.0,
        bbox_right: [room_size * 2.0, -corridor_width, room_size, corridor_width],
        bbox_left: [room_size * 2.0, -room_size - 128.0, -room_size, room_size],
        right_child: NodeChild::SubSector(1), // Corridor
        left_child: NodeChild::Node(1),
    });

    // Node 1: split between north and south
    nodes.push(Node {
        x: 0.0,
        y: 0.0,
        dx: 1.0,
        dy: 0.0,
        bbox_right: [room_size * 2.0, 0.0, -room_size, room_size],
        bbox_left: [0.0, -room_size - 128.0, -room_size, room_size],
        right_child: NodeChild::Node(2),
        left_child: NodeChild::Node(3),
    });

    // Node 2: main room vs north room
    nodes.push(Node {
        x: 0.0,
        y: room_size,
        dx: 1.0,
        dy: 0.0,
        bbox_right: [room_size * 2.0, room_size, -corridor_width, corridor_width],
        bbox_left: [room_size, -room_size, -room_size, room_size],
        right_child: NodeChild::SubSector(2), // North room
        left_child: NodeChild::SubSector(0),  // Main room
    });

    // Node 3: south area
    nodes.push(Node {
        x: 0.0,
        y: -room_size,
        dx: 1.0,
        dy: 0.0,
        bbox_right: [0.0, -room_size, -room_size, room_size],
        bbox_left: [
            -room_size,
            -room_size - 128.0,
            -corridor_width,
            corridor_width,
        ],
        right_child: NodeChild::SubSector(0), // Main room (partial)
        left_child: NodeChild::SubSector(3),  // South alcove
    });

    // Things: player start
    let things = vec![map::Thing {
        x: 0.0,
        y: 0.0,
        angle: 0.0,
        thing_type: wad_types::THING_PLAYER1,
        flags: wad_types::MTF_EASY | wad_types::MTF_NORMAL | wad_types::MTF_HARD,
    }];

    DoomMap {
        name: "TEST".into(),
        vertices,
        linedefs,
        sidedefs,
        sectors,
        segs,
        subsectors,
        nodes,
        things,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_default_creates_test_map() {
        let engine = DoomEngine::default();
        assert!(engine.map.is_some());
        assert_eq!(engine.player.health, 100);
    }

    #[test]
    fn engine_update_advances_time() {
        let mut engine = DoomEngine::default();
        engine.update(0.1);
        assert!(engine.time > 0.0);
    }

    #[test]
    fn engine_fire_sets_flash() {
        let mut engine = DoomEngine::default();
        engine.fire();
        assert!((engine.fire_flash - 1.0).abs() < 0.01);
    }

    #[test]
    fn engine_toggles() {
        let mut engine = DoomEngine::default();
        assert!(!engine.player.noclip);
        engine.toggle_noclip();
        assert!(engine.player.noclip);
        engine.toggle_noclip();
        assert!(!engine.player.noclip);
    }

    #[test]
    fn test_map_has_player_start() {
        let map = generate_test_map();
        assert!(map.player_start().is_some());
    }

    #[test]
    fn test_map_has_bsp_structure() {
        let map = generate_test_map();
        assert!(!map.nodes.is_empty());
        assert!(!map.subsectors.is_empty());
        assert!(!map.segs.is_empty());
    }

    #[test]
    fn render_to_framebuffer() {
        let mut engine = DoomEngine::default();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        // Should not panic
        assert!(engine.frame > 0);
    }

    // ---- DoomEngine::new() state ----

    #[test]
    fn new_engine_has_no_map() {
        let engine = DoomEngine::new();
        assert!(engine.map.is_none());
        assert_eq!(engine.frame, 0);
        assert_eq!(engine.time, 0.0);
        assert_eq!(engine.fire_flash, 0.0);
    }

    #[test]
    fn new_engine_default_flags() {
        let engine = DoomEngine::new();
        assert!(!engine.show_minimap);
        assert!(!engine.show_perf);
        assert!(engine.show_crosshair);
        assert!(!engine.original_palette);
        assert!(engine.vsync);
        assert!(engine.show_hud);
    }

    // ---- load_test_map ----

    #[test]
    fn load_test_map_sets_map() {
        let mut engine = DoomEngine::new();
        assert!(engine.map.is_none());
        engine.load_test_map();
        assert!(engine.map.is_some());
    }

    #[test]
    fn load_test_map_spawns_player_at_origin() {
        let mut engine = DoomEngine::new();
        engine.load_test_map();
        // spawn(0.0, 0.0, 0.0) sets x=0, y=0
        assert!((engine.player.x).abs() < 0.01);
        assert!((engine.player.y).abs() < 0.01);
    }

    // ---- move_forward / strafe / look ----

    #[test]
    fn move_forward_changes_player_velocity() {
        let mut engine = DoomEngine::default();
        let old_x = engine.player.x;
        engine.move_forward(1.0);
        // After move_forward, player velocity should change (not position directly)
        // We need a tick to apply velocity → position, but velocity should be set
        let speed_sq =
            engine.player.mom_x * engine.player.mom_x + engine.player.mom_y * engine.player.mom_y;
        assert!(
            speed_sq > 0.0 || engine.player.x != old_x,
            "move_forward should affect player"
        );
    }

    #[test]
    fn strafe_changes_player_velocity() {
        let mut engine = DoomEngine::default();
        engine.strafe(1.0);
        let speed_sq =
            engine.player.mom_x * engine.player.mom_x + engine.player.mom_y * engine.player.mom_y;
        assert!(speed_sq > 0.0, "strafe should add velocity");
    }

    #[test]
    fn look_changes_player_angle() {
        let mut engine = DoomEngine::default();
        let original_angle = engine.player.angle;
        engine.look(0.5, 0.0);
        assert!(
            (engine.player.angle - original_angle).abs() > 0.01,
            "look should change angle"
        );
    }

    // ---- toggle_god_mode / toggle_run ----

    #[test]
    fn toggle_god_mode_flips() {
        let mut engine = DoomEngine::default();
        assert!(!engine.player.god_mode);
        engine.toggle_god_mode();
        assert!(engine.player.god_mode);
        engine.toggle_god_mode();
        assert!(!engine.player.god_mode);
    }

    #[test]
    fn toggle_run_flips() {
        let mut engine = DoomEngine::default();
        assert!(!engine.player.running);
        engine.toggle_run();
        assert!(engine.player.running);
        engine.toggle_run();
        assert!(!engine.player.running);
    }

    // ---- update / game tick ----

    #[test]
    fn update_accumulates_time() {
        let mut engine = DoomEngine::default();
        engine.update(0.01);
        engine.update(0.02);
        assert!((engine.time - 0.03).abs() < 1e-6);
    }

    #[test]
    fn update_fires_game_tick_when_enough_time() {
        let mut engine = DoomEngine::default();
        engine.player.mom_x = 100.0;
        // Update with enough time for at least one game tick (DOOM_TICK_SECS)
        engine.update(DOOM_TICK_SECS + 0.001);
        // After a tick, player movement should have been processed
        // (velocity + friction + position update via the map)
    }

    #[test]
    fn fire_flash_decays_over_time() {
        let mut engine = DoomEngine::default();
        engine.fire();
        assert!((engine.fire_flash - 1.0).abs() < 0.01);
        engine.update(0.5);
        assert!(
            engine.fire_flash < 1.0,
            "flash should decay: {}",
            engine.fire_flash
        );
    }

    #[test]
    fn fire_flash_reaches_zero_eventually() {
        let mut engine = DoomEngine::default();
        engine.fire();
        for _ in 0..100 {
            engine.update(0.05);
        }
        assert!(
            engine.fire_flash.abs() < 0.01,
            "flash should reach ~0 after 5s: {}",
            engine.fire_flash
        );
    }

    // ---- render with no map ----

    #[test]
    fn render_without_map_does_not_panic() {
        let mut engine = DoomEngine::new();
        // No map loaded — should call render_no_map
        let mut painter = Painter::new(120, 80, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 60, 20, 1);
        assert_eq!(engine.frame, 1);
    }

    // ---- render increments frame ----

    #[test]
    fn render_increments_frame_counter() {
        let mut engine = DoomEngine::default();
        let mut painter = Painter::new(120, 80, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 60, 20, 1);
        assert_eq!(engine.frame, 1);
        engine.render(&mut painter, 60, 20, 1);
        assert_eq!(engine.frame, 2);
    }

    // ---- render with crosshair disabled ----

    #[test]
    fn render_with_crosshair_disabled() {
        let mut engine = DoomEngine {
            show_crosshair: false,
            ..DoomEngine::default()
        };
        let mut painter = Painter::new(120, 80, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 60, 20, 1);
        assert_eq!(engine.frame, 1);
    }

    // ---- render with minimap enabled ----

    #[test]
    fn render_with_minimap_enabled() {
        let mut engine = DoomEngine {
            show_minimap: true,
            ..DoomEngine::default()
        };
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 1);
    }

    // ---- render with muzzle flash ----

    #[test]
    fn render_with_muzzle_flash() {
        let mut engine = DoomEngine::default();
        engine.fire();
        let mut painter = Painter::new(240, 160, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 120, 40, 1);
        assert_eq!(engine.frame, 1);
    }

    // ---- draw_line_fb (Bresenham) ----

    #[test]
    fn draw_line_horizontal() {
        let mut fb = DoomFramebuffer::new(20, 10);
        let color = PackedRgba::rgb(255, 0, 0);
        draw_line_fb(&mut fb, 2, 5, 8, 5, color);
        // All pixels on the line should be set
        for x in 2..=8 {
            assert_eq!(fb.get_pixel(x, 5), color, "pixel at ({x}, 5) should be set");
        }
    }

    #[test]
    fn draw_line_vertical() {
        let mut fb = DoomFramebuffer::new(10, 20);
        let color = PackedRgba::rgb(0, 255, 0);
        draw_line_fb(&mut fb, 5, 2, 5, 8, color);
        for y in 2..=8 {
            assert_eq!(fb.get_pixel(5, y), color, "pixel at (5, {y}) should be set");
        }
    }

    #[test]
    fn draw_line_single_point() {
        let mut fb = DoomFramebuffer::new(10, 10);
        let color = PackedRgba::rgb(0, 0, 255);
        draw_line_fb(&mut fb, 5, 5, 5, 5, color);
        assert_eq!(fb.get_pixel(5, 5), color);
    }

    #[test]
    fn draw_line_diagonal() {
        let mut fb = DoomFramebuffer::new(10, 10);
        let color = PackedRgba::rgb(255, 255, 0);
        draw_line_fb(&mut fb, 0, 0, 5, 5, color);
        // Start and end should be set
        assert_eq!(fb.get_pixel(0, 0), color);
        assert_eq!(fb.get_pixel(5, 5), color);
    }

    // ---- generate_test_map structure ----

    #[test]
    fn test_map_has_sectors() {
        let map = generate_test_map();
        assert!(!map.sectors.is_empty());
    }

    #[test]
    fn test_map_has_linedefs_and_sidedefs() {
        let map = generate_test_map();
        assert!(!map.linedefs.is_empty());
        assert!(!map.sidedefs.is_empty());
    }

    #[test]
    fn test_map_has_vertices() {
        let map = generate_test_map();
        assert!(!map.vertices.is_empty());
    }

    // ---- Default impl ----

    #[test]
    fn default_engine_has_test_map_loaded() {
        let engine = DoomEngine::default();
        assert!(engine.map.is_some());
        let map = engine.map.as_ref().unwrap();
        assert_eq!(map.name, "TEST");
    }

    // ---- multiple operations sequence ----

    #[test]
    fn full_gameplay_sequence() {
        let mut engine = DoomEngine::default();
        engine.move_forward(1.0);
        engine.strafe(0.5);
        engine.look(0.1, 0.0);
        engine.update(DOOM_TICK_SECS * 3.0);
        engine.fire();
        engine.toggle_run();
        engine.toggle_noclip();
        engine.toggle_god_mode();
        let mut painter = Painter::new(120, 80, crate::canvas::Mode::Braille);
        engine.render(&mut painter, 60, 20, 1);
        assert_eq!(engine.frame, 1);
        assert!(engine.time > 0.0);
        assert!(engine.player.running);
        assert!(engine.player.noclip);
        assert!(engine.player.god_mode);
    }
}
