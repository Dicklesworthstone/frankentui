//! Procedural rasterizer for box-drawing and block-element glyphs.
//!
//! Box-drawing characters rendered via `fillText()` produce visible gaps between
//! adjacent cells because font glyph bounding boxes don't perfectly match cell
//! boundaries. This module intercepts those codepoints and draws them as geometric
//! primitives that fill the exact cell dimensions, guaranteeing seamless joins.
//!
//! Handled Unicode ranges:
//! - U+2500..=U+257F  Box Drawing (128 codepoints)
//! - U+2580..=U+259F  Block Elements (32 codepoints)

use crate::glyph_atlas::{GlyphMetrics, GlyphRaster};

/// Attempt to rasterize a box-drawing or block-element glyph procedurally.
///
/// Returns `Some(GlyphRaster)` for handled codepoints, `None` for everything else.
/// All returned glyphs have `bearing_x=0, bearing_y=height, advance_x=width` so
/// they fill the entire cell with zero gaps.
#[must_use]
pub fn rasterize_builtin(codepoint: u32, width: u16, height: u16) -> Option<GlyphRaster> {
    let w = width.max(1);
    let h = height.max(1);

    match codepoint {
        0x2500..=0x257F => Some(rasterize_box_drawing(codepoint, w, h)),
        0x2580..=0x259F => Some(rasterize_block_element(codepoint, w, h)),
        _ => None,
    }
}

fn cell_metrics(w: u16, h: u16) -> GlyphMetrics {
    GlyphMetrics {
        advance_x: i16::try_from(w).unwrap_or(i16::MAX),
        bearing_x: 0,
        bearing_y: i16::try_from(h).unwrap_or(i16::MAX),
    }
}

// ---------------------------------------------------------------------------
// R8 Canvas: single-channel alpha drawing surface
// ---------------------------------------------------------------------------

struct Canvas {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(w: u16, h: u16) -> Self {
        Self {
            width: w,
            height: h,
            pixels: vec![0u8; (w as usize) * (h as usize)],
        }
    }

    fn set(&mut self, x: u16, y: u16, alpha: u8) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            // Additive blend (saturating) for overlapping strokes.
            self.pixels[idx] = self.pixels[idx].saturating_add(alpha);
        }
    }

    fn fill(&mut self, alpha: u8) {
        self.pixels.fill(alpha);
    }

    fn fill_rect(&mut self, x: u16, y: u16, w: u16, h: u16, alpha: u8) {
        for row in y..y.saturating_add(h).min(self.height) {
            for col in x..x.saturating_add(w).min(self.width) {
                let idx = (row as usize) * (self.width as usize) + (col as usize);
                self.pixels[idx] = self.pixels[idx].saturating_add(alpha);
            }
        }
    }

    /// Horizontal line spanning the full cell width with the given stroke thickness.
    fn draw_h_line(&mut self, cy: u16, stroke: u16) {
        let half = stroke / 2;
        let y0 = cy.saturating_sub(half);
        self.fill_rect(0, y0, self.width, stroke, 0xFF);
    }

    /// Vertical line spanning the full cell height with the given stroke thickness.
    fn draw_v_line(&mut self, cx: u16, stroke: u16) {
        let half = stroke / 2;
        let x0 = cx.saturating_sub(half);
        self.fill_rect(x0, 0, stroke, self.height, 0xFF);
    }

    /// Horizontal segment from `x0` to `x1` (inclusive), centered at `cy`.
    fn draw_h_segment(&mut self, cy: u16, x0: u16, x1: u16, stroke: u16) {
        let half = stroke / 2;
        let y0 = cy.saturating_sub(half);
        let sx = x0.min(x1);
        let ex = x0.max(x1);
        let w = ex.saturating_sub(sx).saturating_add(1);
        self.fill_rect(sx, y0, w, stroke, 0xFF);
    }

    /// Vertical segment from `y0` to `y1` (inclusive), centered at `cx`.
    fn draw_v_segment(&mut self, cx: u16, y0: u16, y1: u16, stroke: u16) {
        let half = stroke / 2;
        let x0 = cx.saturating_sub(half);
        let sy = y0.min(y1);
        let ey = y0.max(y1);
        let h = ey.saturating_sub(sy).saturating_add(1);
        self.fill_rect(x0, sy, stroke, h, 0xFF);
    }

    /// Antialiased quarter-circle arc using distance-field rendering.
    fn draw_arc(&mut self, cx_f: f32, cy_f: f32, radius: f32, stroke: f32, quadrant: Quadrant) {
        let outer_r = radius + stroke / 2.0;
        let inner_r = (radius - stroke / 2.0).max(0.0);

        // Bounding box for the arc quadrant.
        let (min_x, max_x, min_y, max_y) = match quadrant {
            Quadrant::TopLeft => (cx_f - outer_r, cx_f, cy_f - outer_r, cy_f),
            Quadrant::TopRight => (cx_f, cx_f + outer_r, cy_f - outer_r, cy_f),
            Quadrant::BottomLeft => (cx_f - outer_r, cx_f, cy_f, cy_f + outer_r),
            Quadrant::BottomRight => (cx_f, cx_f + outer_r, cy_f, cy_f + outer_r),
        };

        let px_min_x = (min_x.floor() as i32).max(0) as u16;
        let px_max_x = ((max_x.ceil() as i32) as u16).min(self.width.saturating_sub(1));
        let px_min_y = (min_y.floor() as i32).max(0) as u16;
        let px_max_y = ((max_y.ceil() as i32) as u16).min(self.height.saturating_sub(1));

        for py in px_min_y..=px_max_y {
            for px in px_min_x..=px_max_x {
                let dx = (px as f32) + 0.5 - cx_f;
                let dy = (py as f32) + 0.5 - cy_f;
                let dist = (dx * dx + dy * dy).sqrt();

                let outer_alpha = smoothstep(outer_r, outer_r - 1.0, dist);
                let inner_alpha = smoothstep(inner_r, inner_r - 1.0, dist);
                let alpha = outer_alpha - inner_alpha;

                if alpha > 0.0 {
                    let val = (alpha * 255.0).round().min(255.0) as u8;
                    self.set(px, py, val);
                }
            }
        }
    }

    /// Antialiased diagonal line using perpendicular distance.
    fn draw_diagonal(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, stroke: f32) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.001 {
            return;
        }
        // Normal direction.
        let nx = -dy / len;
        let ny = dx / len;
        let half_stroke = stroke / 2.0;

        for py in 0..self.height {
            for px in 0..self.width {
                let pcx = (px as f32) + 0.5 - x0;
                let pcy = (py as f32) + 0.5 - y0;

                // Distance along the line direction.
                let along = (pcx * dx + pcy * dy) / len;
                if along < -half_stroke || along > len + half_stroke {
                    continue;
                }

                // Perpendicular distance.
                let perp = (pcx * nx + pcy * ny).abs();
                let alpha = smoothstep(half_stroke, half_stroke - 1.0, perp);
                if alpha > 0.0 {
                    let val = (alpha * 255.0).round().min(255.0) as u8;
                    self.set(px, py, val);
                }
            }
        }
    }

    fn into_raster(self, metrics: GlyphMetrics) -> GlyphRaster {
        GlyphRaster {
            width: self.width,
            height: self.height,
            pixels: self.pixels,
            metrics,
        }
    }
}

#[derive(Clone, Copy)]
enum Quadrant {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// Stroke sizing
// ---------------------------------------------------------------------------

fn light_stroke(cell_dim: u16) -> u16 {
    (f32::from(cell_dim) / 8.0).round().max(1.0) as u16
}

fn heavy_stroke(cell_dim: u16) -> u16 {
    (f32::from(cell_dim) / 4.0).round().max(2.0) as u16
}

// ---------------------------------------------------------------------------
// Box Drawing (U+2500..=U+257F)
// ---------------------------------------------------------------------------

/// Arm weights: 0=none, 1=light, 2=heavy, 3=double.
#[derive(Clone, Copy, Default)]
struct Arms {
    left: u8,
    right: u8,
    up: u8,
    down: u8,
}

fn rasterize_box_drawing(cp: u32, w: u16, h: u16) -> GlyphRaster {
    let mut canvas = Canvas::new(w, h);
    let metrics = cell_metrics(w, h);
    let cx = w / 2;
    let cy = h / 2;

    match cp {
        // --- Simple lines (U+2500..U+2503) ---
        0x2500 => canvas.draw_h_line(cy, light_stroke(h)), // ─
        0x2501 => canvas.draw_h_line(cy, heavy_stroke(h)), // ━
        0x2502 => canvas.draw_v_line(cx, light_stroke(w)), // │
        0x2503 => canvas.draw_v_line(cx, heavy_stroke(w)), // ┃

        // --- Dashed lines (U+2504..U+250B) ---
        0x2504 => draw_h_dashed(&mut canvas, cy, light_stroke(h), 3), // ┄
        0x2505 => draw_h_dashed(&mut canvas, cy, heavy_stroke(h), 3), // ┅
        0x2506 => draw_v_dashed(&mut canvas, cx, light_stroke(w), 3), // ┆
        0x2507 => draw_v_dashed(&mut canvas, cx, heavy_stroke(w), 3), // ┇
        0x2508 => draw_h_dashed(&mut canvas, cy, light_stroke(h), 4), // ┈
        0x2509 => draw_h_dashed(&mut canvas, cy, heavy_stroke(h), 4), // ┉
        0x250A => draw_v_dashed(&mut canvas, cx, light_stroke(w), 4), // ┊
        0x250B => draw_v_dashed(&mut canvas, cx, heavy_stroke(w), 4), // ┋

        // --- Corners, T-junctions, and crosses (U+250C..U+254B) ---
        0x250C..=0x254B => {
            let arms = decode_junction(cp);
            draw_junction(&mut canvas, arms, w, h);
        }

        // --- More dashed (U+254C..U+254F) ---
        0x254C => draw_h_dashed(&mut canvas, cy, light_stroke(h), 2), // ╌
        0x254D => draw_h_dashed(&mut canvas, cy, heavy_stroke(h), 2), // ╍
        0x254E => draw_v_dashed(&mut canvas, cx, light_stroke(w), 2), // ╎
        0x254F => draw_v_dashed(&mut canvas, cx, heavy_stroke(w), 2), // ╏

        // --- Double-line variants (U+2550..U+256C) ---
        0x2550..=0x256C => {
            let arms = decode_double(cp);
            draw_junction(&mut canvas, arms, w, h);
        }

        // --- Rounded corners (U+256D..U+2570) ---
        0x256D => draw_rounded_corner(&mut canvas, Quadrant::TopLeft, w, h), // ╭
        0x256E => draw_rounded_corner(&mut canvas, Quadrant::TopRight, w, h), // ╮
        0x256F => draw_rounded_corner(&mut canvas, Quadrant::BottomRight, w, h), // ╯
        0x2570 => draw_rounded_corner(&mut canvas, Quadrant::BottomLeft, w, h), // ╰

        // --- Diagonals (U+2571..U+2573) ---
        0x2571 => {
            // ╱ (forward slash: bottom-left to top-right)
            let stroke = light_stroke(w.min(h));
            canvas.draw_diagonal(f32::from(w), 0.0, 0.0, f32::from(h), f32::from(stroke));
        }
        0x2572 => {
            // ╲ (backslash: top-left to bottom-right)
            let stroke = light_stroke(w.min(h));
            canvas.draw_diagonal(0.0, 0.0, f32::from(w), f32::from(h), f32::from(stroke));
        }
        0x2573 => {
            // ╳ (X: both diagonals)
            let stroke = light_stroke(w.min(h));
            canvas.draw_diagonal(f32::from(w), 0.0, 0.0, f32::from(h), f32::from(stroke));
            canvas.draw_diagonal(0.0, 0.0, f32::from(w), f32::from(h), f32::from(stroke));
        }

        // --- Half-lines (U+2574..U+257F) ---
        0x2574..=0x257F => {
            let arms = decode_half_line(cp);
            draw_junction(&mut canvas, arms, w, h);
        }

        _ => {} // Shouldn't reach here given the match in rasterize_builtin.
    }

    canvas.into_raster(metrics)
}

// ---------------------------------------------------------------------------
// Dashed line helpers
// ---------------------------------------------------------------------------

fn draw_h_dashed(canvas: &mut Canvas, cy: u16, stroke: u16, segments: u16) {
    let w = canvas.width;
    let gap = (w / (segments * 2)).max(1);
    let seg_len = w.saturating_sub(gap * (segments - 1)) / segments;

    for i in 0..segments {
        let x0 = i * (seg_len + gap);
        let x1 = (x0 + seg_len).min(w);
        canvas.draw_h_segment(cy, x0, x1.saturating_sub(1), stroke);
    }
}

fn draw_v_dashed(canvas: &mut Canvas, cx: u16, stroke: u16, segments: u16) {
    let h = canvas.height;
    let gap = (h / (segments * 2)).max(1);
    let seg_len = h.saturating_sub(gap * (segments - 1)) / segments;

    for i in 0..segments {
        let y0 = i * (seg_len + gap);
        let y1 = (y0 + seg_len).min(h);
        canvas.draw_v_segment(cx, y0, y1.saturating_sub(1), stroke);
    }
}

// ---------------------------------------------------------------------------
// Junction decoding (U+250C..U+254B): corners, tees, crosses
// ---------------------------------------------------------------------------

/// Decode a box-drawing junction (U+250C..U+254B) into arm weights.
/// Each arm is 0=none, 1=light, 2=heavy.
fn decode_junction(cp: u32) -> Arms {
    // This table maps the 64 codepoints U+250C..U+254B.
    // Layout: (right, down, left, up) weights.
    //
    // The Unicode standard arranges these in a specific pattern:
    // U+250C-U+2513: corners (down+right, down+left)
    // U+2514-U+251B: corners (up+right, up+left)
    // U+251C-U+2523: T-pieces (right+up+down, left variants not here)
    // U+2524-U+252B: T-pieces (left+up+down)
    // U+252C-U+2533: T-pieces (down+left+right)
    // U+2534-U+253B: T-pieces (up+left+right)
    // U+253C-U+254B: crosses (all four arms)

    #[rustfmt::skip]
    static TABLE: &[(u8, u8, u8, u8)] = &[
        // U+250C..U+2513: down-right corners, then down-left corners
        (1,1,0,0), // 250C ┌  right=light, down=light
        (2,1,0,0), // 250D ┍  right=heavy, down=light
        (1,2,0,0), // 250E ┎  right=light, down=heavy
        (2,2,0,0), // 250F ┏  right=heavy, down=heavy
        (0,1,1,0), // 2510 ┐  left=light, down=light
        (0,1,2,0), // 2511 ┑  left=heavy, down=light
        (0,2,1,0), // 2512 ┒  left=light, down=heavy
        (0,2,2,0), // 2513 ┓  left=heavy, down=heavy
        // U+2514..U+251B: up-right corners, then up-left corners
        (1,0,0,1), // 2514 └  right=light, up=light
        (2,0,0,1), // 2515 ┕  right=heavy, up=light
        (1,0,0,2), // 2516 ┖  right=light, up=heavy
        (2,0,0,2), // 2517 ┗  right=heavy, up=heavy
        (0,0,1,1), // 2518 ┘  left=light, up=light
        (0,0,2,1), // 2519 ┙  left=heavy, up=light
        (0,0,1,2), // 251A ┚  left=light, up=heavy
        (0,0,2,2), // 251B ┛  left=heavy, up=heavy
        // U+251C..U+2523: right tees (vertical line + right arm)
        (1,1,0,1), // 251C ├  right=light, down=light, up=light
        (2,1,0,1), // 251D ├  right=heavy, down=light, up=light
        (1,2,0,1), // 251E ┞  right=light, down=heavy, up=light
        (1,1,0,2), // 251F ┟  right=light, down=light, up=heavy
        (1,2,0,2), // 2520 ┠  right=light, down=heavy, up=heavy
        (2,1,0,2), // 2521 ┡  right=heavy, down=light, up=heavy
        (2,2,0,1), // 2522 ┢  right=heavy, down=heavy, up=light
        (2,2,0,2), // 2523 ┣  right=heavy, down=heavy, up=heavy
        // U+2524..U+252B: left tees (vertical line + left arm)
        (0,1,1,1), // 2524 ┤  left=light, down=light, up=light
        (0,1,2,1), // 2525 ┥  left=heavy, down=light, up=light
        (0,2,1,1), // 2526 ┦  left=light, down=heavy, up=light
        (0,1,1,2), // 2527 ┧  left=light, down=light, up=heavy
        (0,2,1,2), // 2528 ┨  left=light, down=heavy, up=heavy
        (0,1,2,2), // 2529 ┩  left=heavy, down=light, up=heavy
        (0,2,2,1), // 252A ┪  left=heavy, down=heavy, up=light
        (0,2,2,2), // 252B ┫  left=heavy, down=heavy, up=heavy
        // U+252C..U+2533: down tees (horizontal line + down arm)
        (1,1,1,0), // 252C ┬  right=light, down=light, left=light
        (1,1,2,0), // 252D ┭  right=light, down=light, left=heavy
        (2,1,1,0), // 252E ┮  right=heavy, down=light, left=light
        (2,1,2,0), // 252F ┯  right=heavy, down=light, left=heavy
        (1,2,1,0), // 2530 ┰  right=light, down=heavy, left=light
        (1,2,2,0), // 2531 ┱  right=light, down=heavy, left=heavy
        (2,2,1,0), // 2532 ┲  right=heavy, down=heavy, left=light
        (2,2,2,0), // 2533 ┳  right=heavy, down=heavy, left=heavy
        // U+2534..U+253B: up tees (horizontal line + up arm)
        (1,0,1,1), // 2534 ┴  right=light, left=light, up=light
        (1,0,2,1), // 2535 ┵  right=light, left=heavy, up=light
        (2,0,1,1), // 2536 ┶  right=heavy, left=light, up=light
        (2,0,2,1), // 2537 ┷  right=heavy, left=heavy, up=light
        (1,0,1,2), // 2538 ┸  right=light, left=light, up=heavy
        (1,0,2,2), // 2539 ┹  right=light, left=heavy, up=heavy
        (2,0,1,2), // 253A ┺  right=heavy, left=light, up=heavy
        (2,0,2,2), // 253B ┻  right=heavy, left=heavy, up=heavy
        // U+253C..U+254B: crosses (all four arms)
        (1,1,1,1), // 253C ┼  all light
        (1,1,2,1), // 253D ┽  left=heavy, rest light
        (2,1,1,1), // 253E ┾  right=heavy, rest light
        (2,1,2,1), // 253F ┿  left+right=heavy, up+down=light
        (1,2,1,1), // 2540 ╀  down=heavy, rest light
        (1,1,1,2), // 2541 ╁  up=heavy, rest light
        (1,2,1,2), // 2542 ╂  up+down=heavy, left+right=light
        (1,1,2,2), // 2543 ╃  left+up=heavy
        (2,2,1,1), // 2544 ╄  right+down=heavy
        (1,2,2,1), // 2545 ╅  left+down=heavy
        (2,1,1,2), // 2546 ╆  right+up=heavy
        (2,2,2,1), // 2547 ╇  left+right+down=heavy
        (2,1,2,2), // 2548 ╈  left+right+up=heavy
        (1,2,2,2), // 2549 ╉  left+up+down=heavy
        (2,2,1,2), // 254A ╊  right+up+down=heavy
        (2,2,2,2), // 254B ╋  all heavy
    ];

    let idx = (cp - 0x250C) as usize;
    if let Some(&(right, down, left, up)) = TABLE.get(idx) {
        Arms {
            left,
            right,
            up,
            down,
        }
    } else {
        Arms::default()
    }
}

/// Decode double-line box-drawing characters (U+2550..U+256C).
fn decode_double(cp: u32) -> Arms {
    // Weight 3 = double lines. Inline match to get each codepoint correct per Unicode.
    let (right, down, left, up) = match cp {
        0x2550 => (3, 0, 3, 0), // ═
        0x2551 => (0, 3, 0, 3), // ║
        0x2552 => (3, 1, 0, 0), // ╒  h=double, down=single
        0x2553 => (1, 3, 0, 0), // ╓  h=single, down=double
        0x2554 => (3, 3, 0, 0), // ╔
        0x2555 => (0, 1, 3, 0), // ╕
        0x2556 => (0, 3, 1, 0), // ╖
        0x2557 => (0, 3, 3, 0), // ╗
        0x2558 => (3, 0, 0, 1), // ╘
        0x2559 => (1, 0, 0, 3), // ╙
        0x255A => (3, 0, 0, 3), // ╚
        0x255B => (0, 0, 3, 1), // ╛
        0x255C => (0, 0, 1, 3), // ╜
        0x255D => (0, 0, 3, 3), // ╝
        0x255E => (3, 1, 0, 1), // ╞ right=double, v=single
        0x255F => (1, 3, 0, 3), // ╟ right=single, v=double
        0x2560 => (3, 3, 0, 3), // ╠
        0x2561 => (0, 1, 3, 1), // ╡ left=double, v=single
        0x2562 => (0, 3, 1, 3), // ╢ left=single, v=double
        0x2563 => (0, 3, 3, 3), // ╣
        0x2564 => (3, 1, 3, 0), // ╤ h=double, down=single
        0x2565 => (1, 3, 1, 0), // ╥ h=single, down=double
        0x2566 => (3, 3, 3, 0), // ╦
        0x2567 => (3, 0, 3, 1), // ╧ h=double, up=single
        0x2568 => (1, 0, 1, 3), // ╨ h=single, up=double
        0x2569 => (3, 0, 3, 3), // ╩
        0x256A => (1, 3, 1, 3), // ╪ h=single, v=double
        0x256B => (3, 1, 3, 1), // ╫ h=double, v=single
        0x256C => (3, 3, 3, 3), // ╬
        _ => (0, 0, 0, 0),
    };

    Arms {
        left,
        right,
        up,
        down,
    }
}

/// Decode half-line stubs (U+2574..U+257F).
fn decode_half_line(cp: u32) -> Arms {
    match cp {
        0x2574 => Arms {
            left: 1,
            ..Arms::default()
        }, // ╴ left light
        0x2575 => Arms {
            up: 1,
            ..Arms::default()
        }, // ╵ up light
        0x2576 => Arms {
            right: 1,
            ..Arms::default()
        }, // ╶ right light
        0x2577 => Arms {
            down: 1,
            ..Arms::default()
        }, // ╷ down light
        0x2578 => Arms {
            left: 2,
            ..Arms::default()
        }, // ╸ left heavy
        0x2579 => Arms {
            up: 2,
            ..Arms::default()
        }, // ╹ up heavy
        0x257A => Arms {
            right: 2,
            ..Arms::default()
        }, // ╺ right heavy
        0x257B => Arms {
            down: 2,
            ..Arms::default()
        }, // ╻ down heavy
        0x257C => Arms {
            left: 1,
            right: 2,
            ..Arms::default()
        }, // ╼ left light, right heavy
        0x257D => Arms {
            up: 1,
            down: 2,
            ..Arms::default()
        }, // ╽ up light, down heavy
        0x257E => Arms {
            left: 2,
            right: 1,
            ..Arms::default()
        }, // ╾ left heavy, right light
        0x257F => Arms {
            up: 2,
            down: 1,
            ..Arms::default()
        }, // ╿ up heavy, down light
        _ => Arms::default(),
    }
}

// ---------------------------------------------------------------------------
// Junction rendering
// ---------------------------------------------------------------------------

fn stroke_for_weight(weight: u8, cell_dim: u16) -> u16 {
    match weight {
        0 => 0,
        1 => light_stroke(cell_dim),
        2 => heavy_stroke(cell_dim),
        3 => light_stroke(cell_dim), // double uses light stroke width
        _ => light_stroke(cell_dim),
    }
}

fn draw_junction(canvas: &mut Canvas, arms: Arms, w: u16, h: u16) {
    let any_double = arms.left == 3 || arms.right == 3 || arms.up == 3 || arms.down == 3;

    if any_double {
        draw_double_junction(canvas, arms, w, h);
        return;
    }

    let cx = w / 2;
    let cy = h / 2;
    let h_stroke = stroke_for_weight(arms.left.max(arms.right), h);
    let v_stroke = stroke_for_weight(arms.up.max(arms.down), w);

    if arms.left > 0 {
        let s = stroke_for_weight(arms.left, h);
        canvas.draw_h_segment(cy, 0, cx + v_stroke / 2, s);
    }
    if arms.right > 0 {
        let s = stroke_for_weight(arms.right, h);
        canvas.draw_h_segment(cy, cx.saturating_sub(v_stroke / 2), w.saturating_sub(1), s);
    }
    if arms.up > 0 {
        let s = stroke_for_weight(arms.up, w);
        canvas.draw_v_segment(cx, 0, cy + h_stroke / 2, s);
    }
    if arms.down > 0 {
        let s = stroke_for_weight(arms.down, w);
        canvas.draw_v_segment(cx, cy.saturating_sub(h_stroke / 2), h.saturating_sub(1), s);
    }
}

fn draw_double_junction(canvas: &mut Canvas, arms: Arms, w: u16, h: u16) {
    let cx = w / 2;
    let cy = h / 2;
    let ls = light_stroke(w.min(h));
    let gap = ls; // Gap between double lines equals stroke width.

    // Offsets for double lines from center.
    let h_off = (gap + ls) / 2; // Vertical offset for horizontal double lines
    let v_off = (gap + ls) / 2; // Horizontal offset for vertical double lines

    // Draw horizontal arms.
    if arms.left == 3 {
        // Double horizontal left: two parallel lines.
        canvas.draw_h_segment(cy.saturating_sub(h_off), 0, cx, ls);
        canvas.draw_h_segment(cy.saturating_add(h_off), 0, cx, ls);
    } else if arms.left > 0 {
        let s = stroke_for_weight(arms.left, h);
        canvas.draw_h_segment(cy, 0, cx, s);
    }

    if arms.right == 3 {
        canvas.draw_h_segment(cy.saturating_sub(h_off), cx, w.saturating_sub(1), ls);
        canvas.draw_h_segment(cy.saturating_add(h_off), cx, w.saturating_sub(1), ls);
    } else if arms.right > 0 {
        let s = stroke_for_weight(arms.right, h);
        canvas.draw_h_segment(cy, cx, w.saturating_sub(1), s);
    }

    // Draw vertical arms.
    if arms.up == 3 {
        canvas.draw_v_segment(cx.saturating_sub(v_off), 0, cy, ls);
        canvas.draw_v_segment(cx.saturating_add(v_off), 0, cy, ls);
    } else if arms.up > 0 {
        let s = stroke_for_weight(arms.up, w);
        canvas.draw_v_segment(cx, 0, cy, s);
    }

    if arms.down == 3 {
        canvas.draw_v_segment(cx.saturating_sub(v_off), cy, h.saturating_sub(1), ls);
        canvas.draw_v_segment(cx.saturating_add(v_off), cy, h.saturating_sub(1), ls);
    } else if arms.down > 0 {
        let s = stroke_for_weight(arms.down, w);
        canvas.draw_v_segment(cx, cy, h.saturating_sub(1), s);
    }

    // Fill in the junction center where double lines meet.
    // This connects the parallel strokes at corners/tees/crosses.
    let has_h_double = arms.left == 3 || arms.right == 3;
    let has_v_double = arms.up == 3 || arms.down == 3;

    if has_h_double && has_v_double {
        // Both directions double: draw the 4 corner connecting pieces.
        // The inner rectangle between the double lines should be empty.
        // Top-left to top-right connecting piece (upper horizontal gap fill).
        canvas.fill_rect(
            cx.saturating_sub(v_off),
            cy.saturating_sub(h_off).saturating_sub(ls / 2),
            v_off * 2 + 1,
            ls,
            0xFF,
        );
        // Bottom connecting piece.
        canvas.fill_rect(
            cx.saturating_sub(v_off),
            cy.saturating_add(h_off).saturating_sub(ls / 2),
            v_off * 2 + 1,
            ls,
            0xFF,
        );
        // Left connecting piece.
        canvas.fill_rect(
            cx.saturating_sub(v_off).saturating_sub(ls / 2),
            cy.saturating_sub(h_off),
            ls,
            h_off * 2 + 1,
            0xFF,
        );
        // Right connecting piece.
        canvas.fill_rect(
            cx.saturating_add(v_off).saturating_sub(ls / 2),
            cy.saturating_sub(h_off),
            ls,
            h_off * 2 + 1,
            0xFF,
        );
    }
}

// ---------------------------------------------------------------------------
// Rounded corners
// ---------------------------------------------------------------------------

fn draw_rounded_corner(canvas: &mut Canvas, quadrant: Quadrant, w: u16, h: u16) {
    let cx_f = f32::from(w) / 2.0;
    let cy_f = f32::from(h) / 2.0;
    let stroke = light_stroke(w.min(h));
    let stroke_f = f32::from(stroke);

    // Generous radius for a visually satisfying curve. Uses the full half-cell
    // dimension so the arc sweeps all the way to the cell edge.
    let r = cx_f.min(cy_f);

    // Tangent-circle approach: the arc center is placed diagonally opposite the
    // curve, at (cx ± r, cy ± r). The quarter-circle is tangent to both arms
    // at their innermost points, giving a seamless connection.
    let (arc_cx, arc_cy) = match quadrant {
        Quadrant::TopLeft => (cx_f + r, cy_f + r),
        Quadrant::TopRight => (cx_f - r, cy_f + r),
        Quadrant::BottomLeft => (cx_f + r, cy_f - r),
        Quadrant::BottomRight => (cx_f - r, cy_f - r),
    };

    // Tangent points: where the arc meets the straight arms.
    // If the tangent point is inside the cell, draw a straight stub from it
    // to the cell edge. If it's at the cell edge already (r == cx or cy),
    // the arc alone reaches the edge — no stub needed.
    let cx = w / 2;
    let cy = h / 2;
    let r_i = r as u16;

    match quadrant {
        Quadrant::TopLeft => {
            // ╭ : arms go right and down, curve in top-left
            let h_tangent_x = cx + r_i; // horizontal tangent x
            if h_tangent_x < w {
                canvas.draw_h_segment(cy, h_tangent_x, w.saturating_sub(1), stroke);
            }
            let v_tangent_y = cy + r_i; // vertical tangent y
            if v_tangent_y < h {
                canvas.draw_v_segment(cx, v_tangent_y, h.saturating_sub(1), stroke);
            }
        }
        Quadrant::TopRight => {
            // ╮ : arms go left and down, curve in top-right
            let h_tangent_x = cx.saturating_sub(r_i);
            if h_tangent_x > 0 {
                canvas.draw_h_segment(cy, 0, h_tangent_x, stroke);
            }
            let v_tangent_y = cy + r_i;
            if v_tangent_y < h {
                canvas.draw_v_segment(cx, v_tangent_y, h.saturating_sub(1), stroke);
            }
        }
        Quadrant::BottomRight => {
            // ╯ : arms go left and up, curve in bottom-right
            let h_tangent_x = cx.saturating_sub(r_i);
            if h_tangent_x > 0 {
                canvas.draw_h_segment(cy, 0, h_tangent_x, stroke);
            }
            let v_tangent_y = cy.saturating_sub(r_i);
            if v_tangent_y > 0 {
                canvas.draw_v_segment(cx, 0, v_tangent_y, stroke);
            }
        }
        Quadrant::BottomLeft => {
            // ╰ : arms go right and up, curve in bottom-left
            let h_tangent_x = cx + r_i;
            if h_tangent_x < w {
                canvas.draw_h_segment(cy, h_tangent_x, w.saturating_sub(1), stroke);
            }
            let v_tangent_y = cy.saturating_sub(r_i);
            if v_tangent_y > 0 {
                canvas.draw_v_segment(cx, 0, v_tangent_y, stroke);
            }
        }
    }

    canvas.draw_arc(arc_cx, arc_cy, r, stroke_f, quadrant);
}

// ---------------------------------------------------------------------------
// Block Elements (U+2580..=U+259F)
// ---------------------------------------------------------------------------

fn rasterize_block_element(cp: u32, w: u16, h: u16) -> GlyphRaster {
    let mut canvas = Canvas::new(w, h);
    let metrics = cell_metrics(w, h);

    match cp {
        // Upper fractional fills (U+2580..U+2588).
        0x2580 => {
            // ▀ upper half
            canvas.fill_rect(0, 0, w, h / 2, 0xFF);
        }
        0x2581 => {
            // ▁ lower 1/8
            let frac = h / 8;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2582 => {
            // ▂ lower 1/4
            let frac = h / 4;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2583 => {
            // ▃ lower 3/8
            let frac = h * 3 / 8;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2584 => {
            // ▄ lower half
            canvas.fill_rect(0, h / 2, w, h - h / 2, 0xFF);
        }
        0x2585 => {
            // ▅ lower 5/8
            let frac = h * 5 / 8;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2586 => {
            // ▆ lower 3/4
            let frac = h * 3 / 4;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2587 => {
            // ▇ lower 7/8
            let frac = h * 7 / 8;
            canvas.fill_rect(0, h.saturating_sub(frac), w, frac, 0xFF);
        }
        0x2588 => {
            // █ full block
            canvas.fill(0xFF);
        }

        // Left fractional fills (U+2589..U+258F).
        0x2589 => {
            // ▉ left 7/8
            let frac = w * 7 / 8;
            canvas.fill_rect(0, 0, frac, h, 0xFF);
        }
        0x258A => {
            // ▊ left 3/4
            let frac = w * 3 / 4;
            canvas.fill_rect(0, 0, frac, h, 0xFF);
        }
        0x258B => {
            // ▋ left 5/8
            let frac = w * 5 / 8;
            canvas.fill_rect(0, 0, frac, h, 0xFF);
        }
        0x258C => {
            // ▌ left half
            canvas.fill_rect(0, 0, w / 2, h, 0xFF);
        }
        0x258D => {
            // ▍ left 3/8
            let frac = w * 3 / 8;
            canvas.fill_rect(0, 0, frac, h, 0xFF);
        }
        0x258E => {
            // ▎ left 1/4
            let frac = w / 4;
            canvas.fill_rect(0, 0, frac, h, 0xFF);
        }
        0x258F => {
            // ▏ left 1/8
            let frac = w / 8;
            canvas.fill_rect(0, 0, frac.max(1), h, 0xFF);
        }

        // Right half (U+2590).
        0x2590 => {
            // ▐ right half
            canvas.fill_rect(w / 2, 0, w - w / 2, h, 0xFF);
        }

        // Shade characters (U+2591..U+2593).
        0x2591 => canvas.fill(64),  // ░ light shade
        0x2592 => canvas.fill(128), // ▒ medium shade
        0x2593 => canvas.fill(192), // ▓ dark shade

        // Upper 1/8 block (U+2594).
        0x2594 => {
            let frac = (h / 8).max(1);
            canvas.fill_rect(0, 0, w, frac, 0xFF);
        }

        // Right 1/8 block (U+2595).
        0x2595 => {
            let frac = (w / 8).max(1);
            canvas.fill_rect(w.saturating_sub(frac), 0, frac, h, 0xFF);
        }

        // Quadrant characters (U+2596..U+259F).
        0x2596..=0x259F => {
            let hw = w / 2;
            let hh = h / 2;
            let rw = w - hw; // Right portion width (handles odd widths).
            let bh = h - hh; // Bottom portion height.
            let bits = quadrant_bits(cp);
            if bits & 0b0001 != 0 {
                canvas.fill_rect(0, hh, hw, bh, 0xFF); // bottom-left
            }
            if bits & 0b0010 != 0 {
                canvas.fill_rect(hw, hh, rw, bh, 0xFF); // bottom-right
            }
            if bits & 0b0100 != 0 {
                canvas.fill_rect(0, 0, hw, hh, 0xFF); // top-left
            }
            if bits & 0b1000 != 0 {
                canvas.fill_rect(hw, 0, rw, hh, 0xFF); // top-right
            }
        }

        _ => {} // Shouldn't happen given the range match.
    }

    canvas.into_raster(metrics)
}

/// Returns a 4-bit mask for quadrant characters U+2596..U+259F.
/// Bit layout: bit0=BL, bit1=BR, bit2=TL, bit3=TR.
fn quadrant_bits(cp: u32) -> u8 {
    match cp {
        0x2596 => 0b0001,                   // ▖ BL
        0x2597 => 0b0010,                   // ▗ BR
        0x2598 => 0b0100,                   // ▘ TL
        0x2599 => 0b0100 | 0b0001 | 0b0010, // ▙ TL+BL+BR
        0x259A => 0b0100 | 0b0010,          // ▚ TL+BR
        0x259B => 0b0100 | 0b1000 | 0b0001, // ▛ TL+TR+BL
        0x259C => 0b0100 | 0b1000 | 0b0010, // ▜ TL+TR+BR
        0x259D => 0b1000,                   // ▝ TR
        0x259E => 0b1000 | 0b0001,          // ▞ TR+BL
        0x259F => 0b1000 | 0b0001 | 0b0010, // ▟ TR+BL+BR
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_ascii() {
        assert!(rasterize_builtin(b'A' as u32, 8, 16).is_none());
        assert!(rasterize_builtin(b' ' as u32, 8, 16).is_none());
        assert!(rasterize_builtin(0x00, 8, 16).is_none());
    }

    #[test]
    fn handles_box_drawing_range() {
        for cp in 0x2500..=0x257F {
            let r = rasterize_builtin(cp, 8, 16);
            assert!(r.is_some(), "codepoint U+{cp:04X} should be handled");
            let r = r.unwrap();
            assert_eq!(r.width, 8);
            assert_eq!(r.height, 16);
            assert_eq!(r.pixels.len(), 8 * 16);
            assert_eq!(r.metrics.bearing_x, 0);
            assert_eq!(r.metrics.bearing_y, 16);
            assert_eq!(r.metrics.advance_x, 8);
        }
    }

    #[test]
    fn handles_block_element_range() {
        for cp in 0x2580..=0x259F {
            let r = rasterize_builtin(cp, 10, 20);
            assert!(r.is_some(), "codepoint U+{cp:04X} should be handled");
            let r = r.unwrap();
            assert_eq!(r.width, 10);
            assert_eq!(r.height, 20);
            assert_eq!(r.pixels.len(), 10 * 20);
        }
    }

    #[test]
    fn full_block_is_all_opaque() {
        let r = rasterize_builtin(0x2588, 8, 16).unwrap();
        assert!(r.pixels.iter().all(|&p| p == 0xFF));
    }

    #[test]
    fn space_outside_range_returns_none() {
        // Characters just outside our handled ranges.
        assert!(rasterize_builtin(0x24FF, 8, 16).is_none());
        assert!(rasterize_builtin(0x25A0, 8, 16).is_none());
    }

    #[test]
    fn horizontal_light_line_has_pixels() {
        let r = rasterize_builtin(0x2500, 16, 32).unwrap(); // ─
        // The center row(s) should have some opaque pixels.
        let cy = 32 / 2;
        let row_start = cy as usize * 16;
        let row = &r.pixels[row_start..row_start + 16];
        assert!(row.contains(&0xFF), "center row should have opaque pixels");
    }

    #[test]
    fn vertical_light_line_has_pixels() {
        let r = rasterize_builtin(0x2502, 16, 32).unwrap(); // │
        let cx = 16 / 2;
        // Check center column has opaque pixels.
        let has_opaque = (0..32).any(|y| r.pixels[y * 16 + cx as usize] == 0xFF);
        assert!(has_opaque, "center column should have opaque pixels");
    }

    #[test]
    fn shade_chars_are_uniform() {
        let r1 = rasterize_builtin(0x2591, 8, 16).unwrap(); // ░
        assert!(r1.pixels.iter().all(|&p| p == 64));
        let r2 = rasterize_builtin(0x2592, 8, 16).unwrap(); // ▒
        assert!(r2.pixels.iter().all(|&p| p == 128));
        let r3 = rasterize_builtin(0x2593, 8, 16).unwrap(); // ▓
        assert!(r3.pixels.iter().all(|&p| p == 192));
    }

    #[test]
    fn upper_half_block() {
        let r = rasterize_builtin(0x2580, 8, 16).unwrap(); // ▀
        // Top half should be opaque, bottom half should be empty.
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    r.pixels[y * 8 + x],
                    0xFF,
                    "pixel ({x},{y}) should be opaque"
                );
            }
        }
        for y in 8..16 {
            for x in 0..8 {
                assert_eq!(r.pixels[y * 8 + x], 0, "pixel ({x},{y}) should be empty");
            }
        }
    }

    #[test]
    fn lower_half_block() {
        let r = rasterize_builtin(0x2584, 8, 16).unwrap(); // ▄
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(r.pixels[y * 8 + x], 0, "pixel ({x},{y}) should be empty");
            }
        }
        for y in 8..16 {
            for x in 0..8 {
                assert_eq!(
                    r.pixels[y * 8 + x],
                    0xFF,
                    "pixel ({x},{y}) should be opaque"
                );
            }
        }
    }

    #[test]
    fn left_half_block() {
        let r = rasterize_builtin(0x258C, 8, 16).unwrap(); // ▌
        for y in 0..16 {
            for x in 0..4 {
                assert_eq!(
                    r.pixels[y * 8 + x],
                    0xFF,
                    "pixel ({x},{y}) should be opaque"
                );
            }
            for x in 4..8 {
                assert_eq!(r.pixels[y * 8 + x], 0, "pixel ({x},{y}) should be empty");
            }
        }
    }

    #[test]
    fn right_half_block() {
        let r = rasterize_builtin(0x2590, 8, 16).unwrap(); // ▐
        for y in 0..16 {
            for x in 0..4 {
                assert_eq!(r.pixels[y * 8 + x], 0, "pixel ({x},{y}) should be empty");
            }
            for x in 4..8 {
                assert_eq!(
                    r.pixels[y * 8 + x],
                    0xFF,
                    "pixel ({x},{y}) should be opaque"
                );
            }
        }
    }

    #[test]
    fn quadrant_bottom_left() {
        let r = rasterize_builtin(0x2596, 8, 16).unwrap(); // ▖
        // Bottom-left quadrant: x in [0..4), y in [8..16)
        for y in 8..16 {
            for x in 0..4 {
                assert_eq!(r.pixels[y * 8 + x], 0xFF, "BL pixel ({x},{y})");
            }
        }
        // Top-right should be empty.
        for y in 0..8 {
            for x in 4..8 {
                assert_eq!(r.pixels[y * 8 + x], 0, "TR pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn corner_top_left_has_pixels_in_both_arms() {
        let r = rasterize_builtin(0x250C, 16, 32).unwrap(); // ┌
        let cx = 8usize;
        let cy = 16usize;
        // Check right arm: row at cy, pixels from cx to right edge.
        let right_opaque = (cx..16).any(|x| r.pixels[cy * 16 + x] == 0xFF);
        assert!(right_opaque, "right arm should have opaque pixels");
        // Check down arm: column at cx, pixels from cy to bottom edge.
        let down_opaque = (cy..32).any(|y| r.pixels[y * 16 + cx] == 0xFF);
        assert!(down_opaque, "down arm should have opaque pixels");
    }

    #[test]
    fn minimum_size_does_not_panic() {
        for cp in 0x2500..=0x259F {
            let r = rasterize_builtin(cp, 1, 1);
            assert!(r.is_some());
            assert_eq!(r.unwrap().pixels.len(), 1);
        }
    }

    #[test]
    fn zero_size_clamped_to_one() {
        let r = rasterize_builtin(0x2500, 0, 0).unwrap();
        assert_eq!(r.width, 1);
        assert_eq!(r.height, 1);
    }

    #[test]
    fn diagonal_has_nonzero_pixels() {
        let r = rasterize_builtin(0x2571, 16, 32).unwrap(); // ╱
        let nonzero = r.pixels.iter().filter(|&&p| p > 0).count();
        assert!(nonzero > 0, "diagonal should produce visible pixels");
    }

    #[test]
    fn rounded_corner_has_nonzero_pixels() {
        for cp in 0x256D..=0x2570 {
            let r = rasterize_builtin(cp, 16, 32).unwrap();
            let nonzero = r.pixels.iter().filter(|&&p| p > 0).count();
            assert!(nonzero > 0, "U+{cp:04X} should produce visible pixels");
        }
    }

    #[test]
    fn double_horizontal_has_two_bands() {
        let r = rasterize_builtin(0x2550, 16, 32).unwrap(); // ═
        let cy = 16;
        // Should have opaque pixels above and below center.
        let above = (0..cy).any(|y| (0..16).any(|x| r.pixels[y * 16 + x] == 0xFF));
        let below = (cy..32).any(|y| (0..16).any(|x| r.pixels[y * 16 + x] == 0xFF));
        assert!(above, "double horizontal should have upper band");
        assert!(below, "double horizontal should have lower band");
    }

    #[test]
    fn dashed_has_gaps() {
        let r = rasterize_builtin(0x2504, 32, 16).unwrap(); // ┄ (triple dash)
        let cy = 8;
        let row_start = cy * 32;
        let row = &r.pixels[row_start..row_start + 32];
        // Should have both opaque and empty pixels in the center row.
        let has_opaque = row.contains(&0xFF);
        let has_empty = row.contains(&0);
        assert!(has_opaque, "dashed line should have opaque segments");
        assert!(has_empty, "dashed line should have gaps");
    }

    #[test]
    fn half_line_left_only() {
        let r = rasterize_builtin(0x2574, 16, 32).unwrap(); // ╴ left stub
        let cy = 16usize;
        let cx = 8usize;
        // Left half of center row should have opaque pixels.
        let left_opaque = (0..cx).any(|x| r.pixels[cy * 16 + x] == 0xFF);
        assert!(left_opaque, "left stub should have pixels on the left");
    }
}
