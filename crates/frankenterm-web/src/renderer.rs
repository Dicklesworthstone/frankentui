//! WebGPU renderer skeleton for FrankenTerm.
//!
//! Implements the architecture from ADR-009: single-pass instanced cell quads
//! driven by a storage buffer of per-cell data. The renderer consumes cell
//! patches (dirty spans) and never reads the Grid directly.
//!
//! This skeleton covers:
//! - WebGPU device initialization + surface configuration
//! - Resize handling (surface reconfiguration + instance buffer growth)
//! - Per-cell background + glyph-atlas foreground sampling
//! - Dirty-span patch updates via `queue.write_buffer` slices
//!
//! Atlas glyph generation currently uses a deterministic procedural rasterizer.
//! GPU metadata wiring follows the glyph placement/metrics contract from
//! bd-lff4p.2.4, so production rasterization can be swapped in without changing
//! shader-side layout math.

#[cfg(any(target_arch = "wasm32", test))]
use crate::glyph_atlas::GlyphPlacement;
#[cfg(target_arch = "wasm32")]
use crate::glyph_atlas::{GlyphMetrics, GlyphRaster};
use std::fmt;

// ---------------------------------------------------------------------------
// Platform-agnostic types (available on all targets for type checking)
// ---------------------------------------------------------------------------

/// Size of one cell's GPU data in bytes (4 × u32 = 16 bytes).
pub const CELL_DATA_BYTES: usize = 16;

/// Size of the uniform buffer in bytes (4 × vec4 = 64 bytes).
#[cfg(any(target_arch = "wasm32", test))]
const UNIFORM_BYTES: usize = 64;

/// Size of one glyph metadata entry in bytes (8 × f32 = 32 bytes).
#[cfg(any(target_arch = "wasm32", test))]
const GLYPH_META_BYTES: usize = 32;

/// Glyph atlas dimensions (R8 texture, power-of-two for straightforward uploads).
#[cfg(target_arch = "wasm32")]
const GLYPH_ATLAS_WIDTH: u16 = 2048;
#[cfg(target_arch = "wasm32")]
const GLYPH_ATLAS_HEIGHT: u16 = 2048;

/// Maximum glyph metadata entries mirrored to the GPU.
///
/// Slot 0 is reserved for "no glyph", so real glyphs start at 1.
#[cfg(target_arch = "wasm32")]
const MAX_GLYPH_SLOTS: usize = 4096;

/// Per-cell data sent to the GPU via a storage buffer.
///
/// Layout matches the WGSL `CellData` struct (4 × u32 = 16 bytes, aligned).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellData {
    /// Background color as packed RGBA (R in high byte, A in low byte).
    pub bg_rgba: u32,
    /// Foreground color as packed RGBA.
    pub fg_rgba: u32,
    /// Glyph identifier (index into atlas metadata; 0 = empty/space).
    pub glyph_id: u32,
    /// Packed attributes:
    /// - bits 0..7: style flags
    /// - bits 8..31: hyperlink ID
    pub attrs: u32,
}

/// Packed `CellData::attrs` low-bit mask for style flags.
pub const CELL_ATTR_STYLE_MASK: u32 = 0xFF;
/// Packed `CellData::attrs` shift for hyperlink IDs.
pub const CELL_ATTR_LINK_SHIFT: u32 = 8;

/// Decode style flags from packed `CellData::attrs`.
#[must_use]
pub const fn cell_attr_style_bits(attrs: u32) -> u32 {
    attrs & CELL_ATTR_STYLE_MASK
}

/// Decode hyperlink ID from packed `CellData::attrs`.
#[must_use]
pub const fn cell_attr_link_id(attrs: u32) -> u32 {
    attrs >> CELL_ATTR_LINK_SHIFT
}

/// Cursor rendering style used by the WebGPU fragment shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CursorStyle {
    None = 0,
    Block = 1,
    Bar = 2,
    Underline = 3,
}

impl CursorStyle {
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Block,
            2 => Self::Bar,
            3 => Self::Underline,
            _ => Self::None,
        }
    }

    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl CellData {
    pub const EMPTY: Self = Self {
        bg_rgba: 0x000000FF,
        fg_rgba: 0xFFFFFFFF,
        glyph_id: 0,
        attrs: 0,
    };

    /// Serialize to 16 little-endian bytes matching the WGSL layout.
    #[must_use]
    pub fn to_bytes(self) -> [u8; CELL_DATA_BYTES] {
        let mut buf = [0u8; CELL_DATA_BYTES];
        buf[0..4].copy_from_slice(&self.bg_rgba.to_le_bytes());
        buf[4..8].copy_from_slice(&self.fg_rgba.to_le_bytes());
        buf[8..12].copy_from_slice(&self.glyph_id.to_le_bytes());
        buf[12..16].copy_from_slice(&self.attrs.to_le_bytes());
        buf
    }
}

impl Default for CellData {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct GlyphMetaEntry {
    uv_min_x: f32,
    uv_min_y: f32,
    uv_max_x: f32,
    uv_max_y: f32,
    layout_min_x: f32,
    layout_min_y: f32,
    layout_max_x: f32,
    layout_max_y: f32,
}

#[cfg(any(target_arch = "wasm32", test))]
impl GlyphMetaEntry {
    #[cfg(target_arch = "wasm32")]
    const EMPTY: Self = Self {
        uv_min_x: 0.0,
        uv_min_y: 0.0,
        uv_max_x: 0.0,
        uv_max_y: 0.0,
        layout_min_x: 0.0,
        layout_min_y: 0.0,
        layout_max_x: 1.0,
        layout_max_y: 1.0,
    };

    #[must_use]
    fn from_placement(
        placement: GlyphPlacement,
        atlas_width: u16,
        atlas_height: u16,
        cell_width_px: u16,
        cell_height_px: u16,
    ) -> Self {
        let inv_w = 1.0f32 / f32::from(atlas_width.max(1));
        let inv_h = 1.0f32 / f32::from(atlas_height.max(1));
        let x0 = f32::from(placement.draw.x) * inv_w;
        let y0 = f32::from(placement.draw.y) * inv_h;
        let x1 = f32::from(placement.draw.x.saturating_add(placement.draw.w)) * inv_w;
        let y1 = f32::from(placement.draw.y.saturating_add(placement.draw.h)) * inv_h;
        let cell_w = f32::from(cell_width_px.max(1));
        let cell_h = f32::from(cell_height_px.max(1));
        let layout_min_x = f32::from(placement.metrics.bearing_x) / cell_w;
        let layout_min_y = (cell_h - f32::from(placement.metrics.bearing_y)) / cell_h;
        let layout_max_x = layout_min_x + (f32::from(placement.draw.w) / cell_w);
        let layout_max_y = layout_min_y + (f32::from(placement.draw.h) / cell_h);

        Self {
            uv_min_x: x0.clamp(0.0, 1.0),
            uv_min_y: y0.clamp(0.0, 1.0),
            uv_max_x: x1.clamp(0.0, 1.0),
            uv_max_y: y1.clamp(0.0, 1.0),
            layout_min_x,
            layout_min_y,
            layout_max_x,
            layout_max_y,
        }
    }

    #[must_use]
    fn to_bytes(self) -> [u8; GLYPH_META_BYTES] {
        let mut out = [0u8; GLYPH_META_BYTES];
        out[0..4].copy_from_slice(&self.uv_min_x.to_le_bytes());
        out[4..8].copy_from_slice(&self.uv_min_y.to_le_bytes());
        out[8..12].copy_from_slice(&self.uv_max_x.to_le_bytes());
        out[12..16].copy_from_slice(&self.uv_max_y.to_le_bytes());
        out[16..20].copy_from_slice(&self.layout_min_x.to_le_bytes());
        out[20..24].copy_from_slice(&self.layout_min_y.to_le_bytes());
        out[24..28].copy_from_slice(&self.layout_max_x.to_le_bytes());
        out[28..32].copy_from_slice(&self.layout_max_y.to_le_bytes());
        out
    }
}

#[cfg(target_arch = "wasm32")]
#[must_use]
fn glyph_meta_to_bytes(meta: &[GlyphMetaEntry]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(meta.len() * GLYPH_META_BYTES);
    for entry in meta {
        bytes.extend_from_slice(&entry.to_bytes());
    }
    bytes
}

#[cfg(target_arch = "wasm32")]
fn rasterize_procedural_glyph(codepoint: u32, width: u16, height: u16) -> GlyphRaster {
    let w = width.max(1);
    let h = height.max(1);
    let mut pixels = vec![0u8; (w as usize) * (h as usize)];

    let Some(ch) = char::from_u32(codepoint) else {
        return GlyphRaster {
            width: w,
            height: h,
            pixels,
            metrics: GlyphMetrics {
                advance_x: i16::try_from(w).unwrap_or(i16::MAX),
                bearing_x: 0,
                bearing_y: i16::try_from(h).unwrap_or(i16::MAX),
            },
        };
    };

    if !ch.is_whitespace() {
        let seed = codepoint.wrapping_mul(0x9E37_79B9) ^ (u32::from(w) << 16) ^ u32::from(h);
        for y in 0..h {
            for x in 0..w {
                let border = x == 0 || y == 0 || x + 1 == w || y + 1 == h;
                let bit_index = (u32::from(x) + u32::from(y) * 7) & 31;
                let hash_bit = ((seed >> bit_index) & 1) == 1;
                let stripe = (u32::from(x) * 3 + u32::from(y) + seed).is_multiple_of(11);
                let dot = (u32::from(x) + u32::from(y) * 5 + seed).is_multiple_of(17);
                if border || (hash_bit && stripe) || dot {
                    pixels[(y as usize) * (w as usize) + (x as usize)] = 0xFF;
                }
            }
        }
    }

    GlyphRaster {
        width: w,
        height: h,
        pixels,
        metrics: GlyphMetrics {
            advance_x: i16::try_from(w).unwrap_or(i16::MAX),
            bearing_x: 0,
            bearing_y: i16::try_from(h).unwrap_or(i16::MAX),
        },
    }
}

/// Configuration for renderer initialization.
#[derive(Debug, Clone)]
pub struct RendererConfig {
    /// Cell width in CSS pixels.
    pub cell_width: u16,
    /// Cell height in CSS pixels.
    pub cell_height: u16,
    /// Device pixel ratio (e.g. 2.0 for Retina).
    pub dpr: f32,
    /// User-controlled zoom multiplier (1.0 = default size).
    pub zoom: f32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            cell_width: 8,
            cell_height: 16,
            dpr: 1.0,
            zoom: 1.0,
        }
    }
}

const MIN_DPR: f32 = 0.25;
const MAX_DPR: f32 = 8.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;

/// Deterministic grid/cell geometry derived from CSS metrics + DPR + zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub cell_width_px: f32,
    pub cell_height_px: f32,
    pub dpr: f32,
    pub zoom: f32,
}

fn normalized_scale(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(min, max)
    } else {
        fallback
    }
}

/// Compute deterministic pixel geometry for an existing grid size.
#[must_use]
pub fn grid_geometry(
    cols: u16,
    rows: u16,
    cell_width_css: u16,
    cell_height_css: u16,
    dpr: f32,
    zoom: f32,
) -> GridGeometry {
    let dpr = normalized_scale(dpr, 1.0, MIN_DPR, MAX_DPR);
    let zoom = normalized_scale(zoom, 1.0, MIN_ZOOM, MAX_ZOOM);
    let cell_width_px = (f32::from(cell_width_css.max(1)) * dpr * zoom)
        .round()
        .max(1.0);
    let cell_height_px = (f32::from(cell_height_css.max(1)) * dpr * zoom)
        .round()
        .max(1.0);
    let pixel_width = (f32::from(cols) * cell_width_px).round() as u32;
    let pixel_height = (f32::from(rows) * cell_height_px).round() as u32;

    GridGeometry {
        cols,
        rows,
        pixel_width,
        pixel_height,
        cell_width_px,
        cell_height_px,
        dpr,
        zoom,
    }
}

/// Compute fit-to-container geometry (xterm-fit style) in a deterministic way.
///
/// Uses floor division in device pixels so the computed grid never exceeds the
/// available pixel container.
#[must_use]
pub fn fit_grid_to_container(
    container_width_css: u32,
    container_height_css: u32,
    cell_width_css: u16,
    cell_height_css: u16,
    dpr: f32,
    zoom: f32,
) -> GridGeometry {
    let dpr = normalized_scale(dpr, 1.0, MIN_DPR, MAX_DPR);
    let zoom = normalized_scale(zoom, 1.0, MIN_ZOOM, MAX_ZOOM);
    let cell_width_px = (f32::from(cell_width_css.max(1)) * dpr * zoom)
        .round()
        .max(1.0);
    let cell_height_px = (f32::from(cell_height_css.max(1)) * dpr * zoom)
        .round()
        .max(1.0);

    let container_width_px = ((container_width_css as f32) * dpr)
        .round()
        .max(cell_width_px);
    let container_height_px = ((container_height_css as f32) * dpr)
        .round()
        .max(cell_height_px);

    let cols = (container_width_px / cell_width_px)
        .floor()
        .clamp(1.0, f32::from(u16::MAX)) as u16;
    let rows = (container_height_px / cell_height_px)
        .floor()
        .clamp(1.0, f32::from(u16::MAX)) as u16;

    grid_geometry(cols, rows, cell_width_css, cell_height_css, dpr, zoom)
}

/// Frame statistics returned after each render pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub instance_count: u32,
    pub dirty_cells: u32,
}

/// Renderer initialization or frame errors.
#[derive(Debug, Clone)]
pub enum RendererError {
    /// WebGPU adapter not available.
    NoAdapter,
    /// Device request failed.
    DeviceError(String),
    /// Surface configuration failed.
    SurfaceError(String),
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter => write!(f, "WebGPU adapter not available"),
            Self::DeviceError(msg) => write!(f, "WebGPU device error: {msg}"),
            Self::SurfaceError(msg) => write!(f, "WebGPU surface error: {msg}"),
        }
    }
}

impl std::error::Error for RendererError {}

/// A contiguous span of dirty cells to update on the GPU.
#[derive(Debug, Clone)]
pub struct CellPatch {
    /// Linear offset into the cell grid (row * cols + col).
    pub offset: u32,
    /// Cell data for each cell in the span.
    pub cells: Vec<CellData>,
}

/// Number of `u32` words in one flat patch cell payload (`bg`, `fg`, `glyph`, `attrs`).
pub const FLAT_PATCH_CELL_WORDS: usize = 4;
/// Number of `u32` words in one flat patch span payload (`offset`, `len`).
pub const FLAT_PATCH_SPAN_WORDS: usize = 2;

/// Decode one flat `u32` span into a [`CellPatch`].
///
/// The `flat_cells` payload must be in `[bg, fg, glyph, attrs]` order.
pub fn cell_patch_from_flat_u32(
    offset: u32,
    flat_cells: &[u32],
) -> Result<CellPatch, &'static str> {
    if !flat_cells.len().is_multiple_of(FLAT_PATCH_CELL_WORDS) {
        return Err("flat cell payload length must be a multiple of 4");
    }
    let mut cells = Vec::with_capacity(flat_cells.len() / FLAT_PATCH_CELL_WORDS);
    for chunk in flat_cells.chunks_exact(FLAT_PATCH_CELL_WORDS) {
        cells.push(CellData {
            bg_rgba: chunk[0],
            fg_rgba: chunk[1],
            glyph_id: chunk[2],
            attrs: chunk[3],
        });
    }
    Ok(CellPatch { offset, cells })
}

/// Decode a full flat patch batch into [`CellPatch`] runs.
///
/// - `spans`: `[offset, len, offset, len, ...]`, where `len` is in cells
/// - `flat_cells`: `[bg, fg, glyph, attrs, ...]`
pub fn cell_patches_from_flat_u32(
    spans: &[u32],
    flat_cells: &[u32],
) -> Result<Vec<CellPatch>, &'static str> {
    if !spans.len().is_multiple_of(FLAT_PATCH_SPAN_WORDS) {
        return Err("flat span payload length must be even");
    }
    if !flat_cells.len().is_multiple_of(FLAT_PATCH_CELL_WORDS) {
        return Err("flat cell payload length must be a multiple of 4");
    }

    let mut patches = Vec::with_capacity(spans.len() / FLAT_PATCH_SPAN_WORDS);
    let mut cursor = 0usize;
    for span in spans.chunks_exact(FLAT_PATCH_SPAN_WORDS) {
        let offset = span[0];
        let len_cells = span[1] as usize;
        let words = len_cells
            .checked_mul(FLAT_PATCH_CELL_WORDS)
            .ok_or("flat span length overflow")?;
        let end = cursor
            .checked_add(words)
            .ok_or("flat span cursor overflow")?;
        if end > flat_cells.len() {
            return Err("flat spans exceed provided cell payload");
        }
        patches.push(cell_patch_from_flat_u32(offset, &flat_cells[cursor..end])?);
        cursor = end;
    }

    if cursor != flat_cells.len() {
        return Err("flat spans do not consume entire cell payload");
    }

    Ok(patches)
}

// ---------------------------------------------------------------------------
// WGSL shader (inline)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
const CELL_SHADER_WGSL: &str = r#"
struct Uniforms {
    // (viewport_width, viewport_height, cell_width, cell_height)
    viewport: vec4<f32>,
    // (cols, rows, 0, 0)
    grid: vec4<u32>,
    // (hovered_link_id, cursor_offset, cursor_style, selection_active)
    interaction0: vec4<u32>,
    // (selection_start, selection_end_exclusive, 0, 0)
    interaction1: vec4<u32>,
}

struct CellData {
    bg_rgba: u32,
    fg_rgba: u32,
    glyph_id: u32,
    attrs: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<storage, read> cells: array<CellData>;
@group(0) @binding(2) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(3) var glyph_sampler: sampler;

struct GlyphMeta {
    // UV coordinates in normalized atlas space.
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    // Cell-local glyph layout box in normalized coordinates.
    // This allows proper placement for non-cell-sized raster bounds.
    layout_min: vec2<f32>,
    layout_max: vec2<f32>,
}

@group(0) @binding(4) var<storage, read> glyph_meta: array<GlyphMeta>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) bg_rgba: u32,
    @location(2) @interpolate(flat) fg_rgba: u32,
    @location(3) @interpolate(flat) attrs: u32,
    @location(4) @interpolate(flat) glyph_id: u32,
    @location(5) @interpolate(flat) cell_index: u32,
}

const ATTR_BOLD: u32 = 1u << 0u;
const ATTR_DIM: u32 = 1u << 1u;
const ATTR_ITALIC: u32 = 1u << 2u;
const ATTR_UNDERLINE: u32 = 1u << 3u;
const ATTR_BLINK: u32 = 1u << 4u;
const ATTR_REVERSE: u32 = 1u << 5u;
const ATTR_STRIKETHROUGH: u32 = 1u << 6u;
const ATTR_HIDDEN: u32 = 1u << 7u;

fn unpack_rgba(packed: u32) -> vec4<f32> {
    let r = f32((packed >> 24u) & 0xFFu) / 255.0;
    let g = f32((packed >> 16u) & 0xFFu) / 255.0;
    let b = f32((packed >> 8u) & 0xFFu) / 255.0;
    let a = f32(packed & 0xFFu) / 255.0;
    return vec4<f32>(r, g, b, a);
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let cols = uniforms.grid.x;
    let col = instance_index % cols;
    let row = instance_index / cols;

    // 6 vertices per quad (2 triangles).
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );

    let q = quad[vertex_index];
    let px_x = (f32(col) + q.x) * uniforms.viewport.z;
    let px_y = (f32(row) + q.y) * uniforms.viewport.w;

    let clip_x = (px_x / uniforms.viewport.x) * 2.0 - 1.0;
    let clip_y = 1.0 - (px_y / uniforms.viewport.y) * 2.0;

    let cell = cells[instance_index];

    var out: VertexOutput;
    out.position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.uv = q;
    out.bg_rgba = cell.bg_rgba;
    out.fg_rgba = cell.fg_rgba;
    out.attrs = cell.attrs;
    out.glyph_id = cell.glyph_id;
    out.cell_index = instance_index;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let style = in.attrs & 0xFFu;
    let link_id = in.attrs >> 8u;
    var bg = unpack_rgba(in.bg_rgba);
    var fg = unpack_rgba(in.fg_rgba);

    if ((style & ATTR_REVERSE) != 0u) {
        let tmp = bg;
        bg = fg;
        fg = tmp;
    }

    let selection_active = uniforms.interaction0.w != 0u;
    let selection_start = uniforms.interaction1.x;
    let selection_end = uniforms.interaction1.y;
    let selected = selection_active
        && selection_start < selection_end
        && in.cell_index >= selection_start
        && in.cell_index < selection_end;
    if (selected) {
        let tmp = bg;
        bg = fg;
        fg = tmp;
    }

    let cursor_offset = uniforms.interaction0.y;
    let cursor_style = uniforms.interaction0.z;
    let cursor_here = cursor_style != 0u && in.cell_index == cursor_offset;
    if (cursor_here && cursor_style == 1u) {
        let tmp = bg;
        bg = fg;
        fg = tmp;
    }

    if ((style & ATTR_DIM) != 0u) {
        fg = vec4<f32>(fg.rgb * 0.6, fg.a);
    }
    if ((style & ATTR_BOLD) != 0u) {
        fg = vec4<f32>(min(fg.rgb * 1.2, vec3<f32>(1.0, 1.0, 1.0)), fg.a);
    }
    if ((style & ATTR_BLINK) != 0u) {
        fg = vec4<f32>(fg.rgb, fg.a * 0.85);
    }
    if ((style & ATTR_HIDDEN) != 0u) {
        fg = vec4<f32>(fg.rgb, 0.0);
    }

    var uv = in.uv;
    if ((style & ATTR_ITALIC) != 0u) {
        uv.x = clamp(uv.x + (0.5 - uv.y) * 0.18, 0.0, 1.0);
    }

    let underline = (style & ATTR_UNDERLINE) != 0u && in.uv.y >= 0.90;
    let strike = (style & ATTR_STRIKETHROUGH) != 0u
        && abs(in.uv.y - 0.55) <= 0.03;
    let hover_underline = uniforms.interaction0.x != 0u
        && link_id == uniforms.interaction0.x
        && in.uv.y >= 0.90;
    let cursor_bar = cursor_here && cursor_style == 2u && in.uv.x <= 0.12;
    let cursor_underline = cursor_here && cursor_style == 3u && in.uv.y >= 0.90;

    var glyph_alpha = 0.0;
    if (in.glyph_id != 0u) {
        let glyph_meta_entry = glyph_meta[in.glyph_id];
        if (all(uv >= glyph_meta_entry.layout_min) && all(uv <= glyph_meta_entry.layout_max)) {
            let span = max(
                glyph_meta_entry.layout_max - glyph_meta_entry.layout_min,
                vec2<f32>(0.00001, 0.00001),
            );
            let local_uv = clamp(
                (uv - glyph_meta_entry.layout_min) / span,
                vec2<f32>(0.0, 0.0),
                vec2<f32>(1.0, 1.0),
            );
            let atlas_uv = vec2<f32>(
                mix(glyph_meta_entry.uv_min.x, glyph_meta_entry.uv_max.x, local_uv.x),
                mix(glyph_meta_entry.uv_min.y, glyph_meta_entry.uv_max.y, local_uv.y),
            );
            glyph_alpha = textureSampleLevel(glyph_atlas, glyph_sampler, atlas_uv, 0.0).r;
        }
    }

    let decoration_alpha = select(
        0.0,
        1.0,
        underline || strike || hover_underline || cursor_bar || cursor_underline,
    );
    let ink_alpha = max(glyph_alpha, decoration_alpha) * fg.a;
    let out_rgb = bg.rgb * (1.0 - ink_alpha) + fg.rgb * ink_alpha;
    let out_a = max(bg.a, ink_alpha);
    return vec4<f32>(out_rgb, out_a);
}
"#;

// ---------------------------------------------------------------------------
// wasm32 renderer implementations
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod gpu {
    use super::*;
    use crate::glyph_atlas::{AtlasRect, GlyphAtlasCache, GlyphKey};
    use std::collections::HashMap;
    use wasm_bindgen::JsCast;
    use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};
    use wgpu;

    const ATTR_BOLD: u32 = 1 << 0;
    const ATTR_DIM: u32 = 1 << 1;
    const ATTR_ITALIC: u32 = 1 << 2;
    const ATTR_UNDERLINE: u32 = 1 << 3;
    const ATTR_BLINK: u32 = 1 << 4;
    const ATTR_REVERSE: u32 = 1 << 5;
    const ATTR_STRIKETHROUGH: u32 = 1 << 6;
    const ATTR_HIDDEN: u32 = 1 << 7;

    /// WebGPU renderer owning all GPU resources.
    ///
    /// Follows ADR-009: single pipeline, instanced cell quads, storage-buffer
    /// driven, patch-based updates.
    struct GpuRenderer {
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        surface_config: wgpu::SurfaceConfiguration,
        pipeline: wgpu::RenderPipeline,
        uniform_buffer: wgpu::Buffer,
        cell_buffer: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        bind_group_layout: wgpu::BindGroupLayout,
        glyph_meta_buffer: wgpu::Buffer,
        _atlas_texture: wgpu::Texture,
        atlas_view: wgpu::TextureView,
        atlas_sampler: wgpu::Sampler,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
        dpr: f32,
        zoom: f32,
        atlas_width: u16,
        atlas_height: u16,
        glyph_cache: GlyphAtlasCache,
        glyph_slot_by_key: HashMap<GlyphKey, u32>,
        glyph_meta_cpu: Vec<GlyphMetaEntry>,
        next_glyph_slot: u32,
        /// Shadow copy of cell data for resize-time buffer rebuilds.
        cells_cpu: Vec<CellData>,
        /// Scratch buffer reused for patch uploads to avoid per-patch allocs.
        patch_upload_scratch: Vec<u8>,
        /// Dirty cells uploaded since the previous render call.
        last_dirty_cells: u32,
        /// Hyperlink ID currently hovered by the pointer (0 = none).
        hovered_link_id: u32,
        /// Cursor cell offset for overlay rendering.
        cursor_offset: Option<u32>,
        /// Cursor rendering style.
        cursor_style: CursorStyle,
        /// Selected cell range as `[start, end)` offsets.
        selection_range: Option<(u32, u32)>,
    }

    impl GpuRenderer {
        /// Initialize the WebGPU renderer on the given canvas.
        pub async fn init(
            canvas: HtmlCanvasElement,
            cols: u16,
            rows: u16,
            config: &RendererConfig,
        ) -> Result<Self, RendererError> {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::BROWSER_WEBGPU,
                ..Default::default()
            });

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|_| RendererError::NoAdapter)?;

            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("frankenterm"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                    ..Default::default()
                })
                .await
                .map_err(|e| RendererError::DeviceError(e.to_string()))?;

            // Only touch the canvas after we know we have an adapter/device.
            // This keeps the Canvas2D fallback path viable when WebGPU is unavailable.
            let surface = instance
                .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
                .map_err(|e| RendererError::SurfaceError(e.to_string()))?;

            let geometry = grid_geometry(
                cols,
                rows,
                config.cell_width,
                config.cell_height,
                config.dpr,
                config.zoom,
            );
            let dpr = geometry.dpr;
            let zoom = geometry.zoom;
            let cell_w_px = geometry.cell_width_px;
            let cell_h_px = geometry.cell_height_px;
            let pixel_width = geometry.pixel_width;
            let pixel_height = geometry.pixel_height;

            let surface_caps = surface.get_capabilities(&adapter);
            let format = surface_caps
                .formats
                .first()
                .copied()
                .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);

            let surface_config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: pixel_width.max(1),
                height: pixel_height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: surface_caps
                    .alpha_modes
                    .first()
                    .copied()
                    .unwrap_or(wgpu::CompositeAlphaMode::Auto),
                view_formats: vec![],
            };
            surface.configure(&device, &surface_config);

            // Shader module.
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("cell_shader"),
                source: wgpu::ShaderSource::Wgsl(CELL_SHADER_WGSL.into()),
            });

            // Bind group layout: uniform + cell storage + atlas + glyph metadata.
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("cell_bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 4,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cell_pl"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("cell_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

            // Buffers.
            let cell_count = (cols as usize) * (rows as usize);
            let cells_cpu = vec![CellData::EMPTY; cell_count];
            let cell_bytes = cells_to_bytes(&cells_cpu);

            let cell_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cells"),
                size: (cell_bytes.len().max(CELL_DATA_BYTES)) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !cell_bytes.is_empty() {
                queue.write_buffer(&cell_buffer, 0, &cell_bytes);
            }

            let atlas_width = GLYPH_ATLAS_WIDTH;
            let atlas_height = GLYPH_ATLAS_HEIGHT;
            let glyph_cache = GlyphAtlasCache::new(
                atlas_width,
                atlas_height,
                usize::from(atlas_width) * usize::from(atlas_height),
            );

            let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("glyph_atlas"),
                size: wgpu::Extent3d {
                    width: u32::from(atlas_width),
                    height: u32::from(atlas_height),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("glyph_atlas_sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            });

            let glyph_meta_cpu = vec![GlyphMetaEntry::EMPTY; MAX_GLYPH_SLOTS];
            let glyph_meta_bytes = glyph_meta_to_bytes(&glyph_meta_cpu);
            let glyph_meta_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyph_meta"),
                size: glyph_meta_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&glyph_meta_buffer, 0, &glyph_meta_bytes);

            let uniform_bytes = uniforms_bytes(
                pixel_width as f32,
                pixel_height as f32,
                cell_w_px,
                cell_h_px,
                cols as u32,
                rows as u32,
                InteractionUniforms {
                    hovered_link_id: 0,
                    cursor_offset: u32::MAX,
                    cursor_style: CursorStyle::None.as_u32(),
                    selection_active: 0,
                    selection_start: 0,
                    selection_end: 0,
                },
            );
            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("uniforms"),
                size: UNIFORM_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&uniform_buffer, 0, &uniform_bytes);

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cell_bg"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: cell_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: glyph_meta_buffer.as_entire_binding(),
                    },
                ],
            });

            Ok(Self {
                device,
                queue,
                surface,
                surface_config,
                pipeline,
                uniform_buffer,
                cell_buffer,
                bind_group,
                bind_group_layout,
                glyph_meta_buffer,
                _atlas_texture: atlas_texture,
                atlas_view,
                atlas_sampler,
                cols,
                rows,
                cell_width: config.cell_width,
                cell_height: config.cell_height,
                dpr,
                zoom,
                atlas_width,
                atlas_height,
                glyph_cache,
                glyph_slot_by_key: HashMap::new(),
                glyph_meta_cpu,
                next_glyph_slot: 1,
                cells_cpu,
                patch_upload_scratch: Vec::new(),
                last_dirty_cells: 0,
                hovered_link_id: 0,
                cursor_offset: None,
                cursor_style: CursorStyle::None,
                selection_range: None,
            })
        }

        /// Resize the grid. Reconfigures the surface and rebuilds the cell buffer.
        pub fn resize(&mut self, cols: u16, rows: u16) {
            if cols == self.cols && rows == self.rows {
                return;
            }
            self.cols = cols;
            self.rows = rows;

            let geometry = grid_geometry(
                cols,
                rows,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            );
            let pixel_w = geometry.pixel_width;
            let pixel_h = geometry.pixel_height;

            self.surface_config.width = pixel_w.max(1);
            self.surface_config.height = pixel_h.max(1);
            self.surface.configure(&self.device, &self.surface_config);

            // Rebuild cell buffer for new grid size.
            let cell_count = (cols as usize) * (rows as usize);
            self.cells_cpu.resize(cell_count, CellData::EMPTY);
            let cell_bytes = cells_to_bytes(&self.cells_cpu);

            self.cell_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cells"),
                size: (cell_bytes.len().max(CELL_DATA_BYTES)) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !cell_bytes.is_empty() {
                self.queue.write_buffer(&self.cell_buffer, 0, &cell_bytes);
            }

            // Update uniforms.
            self.write_uniforms_for_geometry(geometry);

            // Recreate bind group (cell_buffer changed).
            self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cell_bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.cell_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.atlas_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: self.glyph_meta_buffer.as_entire_binding(),
                    },
                ],
            });
        }

        #[must_use]
        pub fn dpr(&self) -> f32 {
            self.dpr
        }

        #[must_use]
        pub fn zoom(&self) -> f32 {
            self.zoom
        }

        fn grid_cell_capacity(&self) -> u32 {
            u32::from(self.cols).saturating_mul(u32::from(self.rows))
        }

        fn clamp_cursor_offset(&self, offset: Option<u32>) -> Option<u32> {
            let max = self.grid_cell_capacity();
            offset.filter(|v| *v < max)
        }

        fn clamp_selection_range(&self, range: Option<(u32, u32)>) -> Option<(u32, u32)> {
            let max = self.grid_cell_capacity();
            let (a, b) = range?;
            let start = a.min(max);
            let end = b.min(max);
            if start == end {
                return None;
            }
            Some((start.min(end), start.max(end)))
        }

        fn write_uniforms_for_geometry(&mut self, geometry: GridGeometry) {
            self.cursor_offset = self.clamp_cursor_offset(self.cursor_offset);
            if self.cursor_offset.is_none() {
                self.cursor_style = CursorStyle::None;
            }
            self.selection_range = self.clamp_selection_range(self.selection_range);

            let (selection_active, selection_start, selection_end) =
                if let Some((start, end)) = self.selection_range {
                    (1u32, start, end)
                } else {
                    (0u32, 0u32, 0u32)
                };

            let ub = uniforms_bytes(
                geometry.pixel_width as f32,
                geometry.pixel_height as f32,
                geometry.cell_width_px,
                geometry.cell_height_px,
                self.cols as u32,
                self.rows as u32,
                InteractionUniforms {
                    hovered_link_id: self.hovered_link_id,
                    cursor_offset: self.cursor_offset.unwrap_or(u32::MAX),
                    cursor_style: self.cursor_style.as_u32(),
                    selection_active,
                    selection_start,
                    selection_end,
                },
            );
            self.queue.write_buffer(&self.uniform_buffer, 0, &ub);
        }

        /// Set currently hovered hyperlink ID (0 clears hover underline).
        pub fn set_hovered_link_id(&mut self, link_id: u32) {
            if self.hovered_link_id == link_id {
                return;
            }
            self.hovered_link_id = link_id;
            self.write_uniforms_for_geometry(self.current_geometry());
        }

        /// Configure cursor overlay state.
        pub fn set_cursor(&mut self, offset: Option<u32>, style: CursorStyle) {
            self.cursor_offset = self.clamp_cursor_offset(offset);
            self.cursor_style = if self.cursor_offset.is_some() {
                style
            } else {
                CursorStyle::None
            };
            self.write_uniforms_for_geometry(self.current_geometry());
        }

        /// Configure selection overlay as a `[start, end)` offset range.
        pub fn set_selection_range(&mut self, range: Option<(u32, u32)>) {
            self.selection_range = self.clamp_selection_range(range);
            self.write_uniforms_for_geometry(self.current_geometry());
        }

        /// Update DPR/zoom while keeping the current grid dimensions.
        pub fn set_scale(&mut self, dpr: f32, zoom: f32) {
            self.dpr = normalized_scale(dpr, 1.0, MIN_DPR, MAX_DPR);
            self.zoom = normalized_scale(zoom, 1.0, MIN_ZOOM, MAX_ZOOM);

            let geometry = grid_geometry(
                self.cols,
                self.rows,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            );
            self.surface_config.width = geometry.pixel_width.max(1);
            self.surface_config.height = geometry.pixel_height.max(1);
            self.surface.configure(&self.device, &self.surface_config);

            self.write_uniforms_for_geometry(geometry);
        }

        #[must_use]
        pub fn current_geometry(&self) -> GridGeometry {
            grid_geometry(
                self.cols,
                self.rows,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            )
        }

        /// Fit the grid to a CSS-pixel container and resize the renderer.
        pub fn fit_to_container(
            &mut self,
            container_width_css: u32,
            container_height_css: u32,
        ) -> GridGeometry {
            let fit = fit_grid_to_container(
                container_width_css,
                container_height_css,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            );
            self.resize(fit.cols, fit.rows);
            fit
        }

        fn glyph_pixel_size(&self) -> (u16, u16) {
            let w = (f32::from(self.cell_width) * self.dpr * self.zoom).round();
            let h = (f32::from(self.cell_height) * self.dpr * self.zoom).round();
            (
                w.clamp(1.0, f32::from(u16::MAX)) as u16,
                h.clamp(1.0, f32::from(u16::MAX)) as u16,
            )
        }

        /// Upload the full atlas texture to the GPU.
        ///
        /// Used as a fallback when partial uploads are not possible (e.g. after
        /// atlas eviction/rebuild).
        #[allow(dead_code)]
        fn upload_full_atlas(&mut self) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self._atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                self.glyph_cache.atlas_pixels(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u32::from(self.atlas_width)),
                    rows_per_image: Some(u32::from(self.atlas_height)),
                },
                wgpu::Extent3d {
                    width: u32::from(self.atlas_width),
                    height: u32::from(self.atlas_height),
                    depth_or_array_layers: 1,
                },
            );
        }

        /// Upload only the dirty atlas rectangles to the GPU.
        ///
        /// Each dirty rect is uploaded as an independent sub-region write,
        /// avoiding the 4 MB full-atlas upload when only a few glyphs changed.
        fn upload_dirty_atlas_rects(&mut self, dirty_rects: &[AtlasRect]) {
            let atlas_w = self.atlas_width as usize;
            let pixels = self.glyph_cache.atlas_pixels();

            for rect in dirty_rects {
                let rw = rect.w as usize;
                let rh = rect.h as usize;
                // Guard: wgpu requires extent > 0 in each dimension.
                if rw == 0 || rh == 0 {
                    continue;
                }
                // Extract the sub-region rows into the scratch buffer.
                self.patch_upload_scratch.clear();
                self.patch_upload_scratch.reserve(rw * rh);
                for row in 0..rh {
                    let src_start = (rect.y as usize + row) * atlas_w + rect.x as usize;
                    self.patch_upload_scratch
                        .extend_from_slice(&pixels[src_start..src_start + rw]);
                }

                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self._atlas_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: u32::from(rect.x),
                            y: u32::from(rect.y),
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &self.patch_upload_scratch,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(rw as u32),
                        rows_per_image: Some(rh as u32),
                    },
                    wgpu::Extent3d {
                        width: rw as u32,
                        height: rh as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        fn ensure_glyph_slot(&mut self, codepoint: u32) -> u32 {
            if codepoint == 0 {
                return 0;
            }

            let Some(ch) = char::from_u32(codepoint) else {
                return 0;
            };
            if ch.is_whitespace() {
                // The background path already renders whitespace cells; avoid
                // wasting atlas/slots on empty glyphs.
                return 0;
            }
            let (cell_w, cell_h) = self.glyph_pixel_size();
            let key = GlyphKey::from_char(ch, cell_h.max(1));

            let existing_slot = self.glyph_slot_by_key.get(&key).copied();
            if existing_slot.is_none() && (self.next_glyph_slot as usize) >= MAX_GLYPH_SLOTS {
                return 0;
            }

            let placement = match self.glyph_cache.get_or_insert_with(key, |_| {
                // Procedural fallback raster must match the per-cell quad size; otherwise the
                // UV mapping would stretch it. The production font rasterizer (bd-lff4p.2.4)
                // is expected to generate cell-sized bitmaps for this pipeline as well.
                rasterize_procedural_glyph(codepoint, cell_w, cell_h)
            }) {
                Ok(placement) => placement,
                Err(_) => return 0,
            };

            let slot = existing_slot.unwrap_or_else(|| {
                let slot = self.next_glyph_slot;
                self.next_glyph_slot = self.next_glyph_slot.saturating_add(1);
                self.glyph_slot_by_key.insert(key, slot);
                slot
            });

            let meta = GlyphMetaEntry::from_placement(
                placement,
                self.atlas_width,
                self.atlas_height,
                cell_w,
                cell_h,
            );
            if self.glyph_meta_cpu[slot as usize] != meta {
                self.glyph_meta_cpu[slot as usize] = meta;
                let byte_offset = (slot as u64) * (GLYPH_META_BYTES as u64);
                self.queue
                    .write_buffer(&self.glyph_meta_buffer, byte_offset, &meta.to_bytes());
            }

            slot
        }

        /// Apply dirty-span cell patches. Only modified cells are uploaded.
        pub fn apply_patches(&mut self, patches: &[CellPatch]) -> u32 {
            let max = (self.cols as u32) * (self.rows as u32);
            let mut dirty = 0u32;

            for patch in patches {
                let start = patch.offset;
                let end = start.saturating_add(patch.cells.len() as u32).min(max);
                if start >= max {
                    continue;
                }

                let count = (end - start) as usize;
                if count == 0 {
                    continue;
                }
                // Upload only the dirty range to the GPU.
                let byte_offset = (start as u64) * (CELL_DATA_BYTES as u64);
                self.patch_upload_scratch.clear();
                self.patch_upload_scratch.reserve(count * CELL_DATA_BYTES);

                for i in 0..count {
                    let mut gpu_cell = patch.cells[i];
                    // Fast-path: skip the glyph atlas lookup for empty/space
                    // cells (~60-80% of typical terminal content).
                    let cp = gpu_cell.glyph_id;
                    gpu_cell.glyph_id = if cp == 0 || cp == b' ' as u32 {
                        0
                    } else {
                        self.ensure_glyph_slot(cp)
                    };
                    self.cells_cpu[(start as usize) + i] = gpu_cell;
                    self.patch_upload_scratch
                        .extend_from_slice(&gpu_cell.to_bytes());
                }
                self.queue
                    .write_buffer(&self.cell_buffer, byte_offset, &self.patch_upload_scratch);
                dirty += count as u32;
            }

            // Upload only the dirty atlas sub-regions instead of the full 4 MB
            // texture. Typically only a handful of small glyph rects change per
            // frame, reducing GPU transfer from O(atlas_size) to O(dirty_area).
            let dirty_rects = self.glyph_cache.take_dirty_rects();
            if !dirty_rects.is_empty() {
                self.upload_dirty_atlas_rects(&dirty_rects);
            }

            self.last_dirty_cells = dirty;
            dirty
        }

        /// Encode and submit one render frame.
        pub fn render_frame(&mut self) -> Result<FrameStats, RendererError> {
            let output = self
                .surface
                .get_current_texture()
                .map_err(|e| RendererError::SurfaceError(e.to_string()))?;

            let view = output
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame"),
                });

            let instance_count = (self.cols as u32) * (self.rows as u32);

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("cell_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                // 6 vertices per cell (2 triangles), instanced per cell.
                pass.draw(0..6, 0..instance_count);
            }

            self.queue.submit(std::iter::once(encoder.finish()));
            output.present();

            let dirty_cells = self.last_dirty_cells;
            self.last_dirty_cells = 0;

            Ok(FrameStats {
                instance_count,
                dirty_cells,
            })
        }

        /// Current grid dimensions.
        #[must_use]
        pub fn grid_size(&self) -> (u16, u16) {
            (self.cols, self.rows)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct RgbaColor {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    }

    impl RgbaColor {
        #[must_use]
        fn from_packed(packed: u32) -> Self {
            Self {
                r: ((packed >> 24) & 0xFF) as u8,
                g: ((packed >> 16) & 0xFF) as u8,
                b: ((packed >> 8) & 0xFF) as u8,
                a: (packed & 0xFF) as u8,
            }
        }

        #[must_use]
        fn scale_rgb(self, factor: f32) -> Self {
            let scale = factor.max(0.0);
            let mut next = self;
            next.r = (f32::from(self.r) * scale).clamp(0.0, 255.0) as u8;
            next.g = (f32::from(self.g) * scale).clamp(0.0, 255.0) as u8;
            next.b = (f32::from(self.b) * scale).clamp(0.0, 255.0) as u8;
            next
        }

        #[must_use]
        fn scale_alpha(self, factor: f32) -> Self {
            let alpha = (f32::from(self.a) * factor.max(0.0)).clamp(0.0, 255.0);
            Self {
                a: alpha as u8,
                ..self
            }
        }

        #[must_use]
        fn with_alpha(self, alpha: f32) -> Self {
            let value = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            Self { a: value, ..self }
        }

        #[must_use]
        fn to_css_rgba(self) -> String {
            format!(
                "rgba({}, {}, {}, {:.6})",
                self.r,
                self.g,
                self.b,
                f32::from(self.a) / 255.0
            )
        }
    }

    struct Canvas2dRenderer {
        canvas: HtmlCanvasElement,
        context: CanvasRenderingContext2d,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
        dpr: f32,
        zoom: f32,
        cells_cpu: Vec<CellData>,
        last_dirty_cells: u32,
        hovered_link_id: u32,
        cursor_offset: Option<u32>,
        cursor_style: CursorStyle,
        selection_range: Option<(u32, u32)>,
    }

    impl Canvas2dRenderer {
        fn init(
            canvas: HtmlCanvasElement,
            cols: u16,
            rows: u16,
            config: &RendererConfig,
        ) -> Result<Self, RendererError> {
            let context = canvas
                .get_context("2d")
                .map_err(|error| {
                    RendererError::SurfaceError(format!(
                        "canvas2d context request failed: {error:?}"
                    ))
                })?
                .ok_or_else(|| {
                    RendererError::SurfaceError(
                        "canvas2d context unavailable on this browser".to_owned(),
                    )
                })?
                .dyn_into::<CanvasRenderingContext2d>()
                .map_err(|error| {
                    RendererError::SurfaceError(format!("canvas2d context cast failed: {error:?}"))
                })?;

            let geometry = grid_geometry(
                cols,
                rows,
                config.cell_width,
                config.cell_height,
                config.dpr,
                config.zoom,
            );
            let mut renderer = Self {
                canvas,
                context,
                cols,
                rows,
                cell_width: config.cell_width,
                cell_height: config.cell_height,
                dpr: geometry.dpr,
                zoom: geometry.zoom,
                cells_cpu: vec![CellData::EMPTY; usize::from(cols) * usize::from(rows)],
                last_dirty_cells: 0,
                hovered_link_id: 0,
                cursor_offset: None,
                cursor_style: CursorStyle::None,
                selection_range: None,
            };
            renderer.sync_canvas_size(geometry);
            renderer.apply_context_defaults();
            Ok(renderer)
        }

        fn apply_context_defaults(&self) {
            self.context.set_text_baseline("top");
            self.context.set_text_align("left");
            self.context.set_image_smoothing_enabled(false);
        }

        #[must_use]
        fn grid_cell_capacity(&self) -> u32 {
            u32::from(self.cols).saturating_mul(u32::from(self.rows))
        }

        #[must_use]
        fn clamp_cursor_offset(&self, offset: Option<u32>) -> Option<u32> {
            let max = self.grid_cell_capacity();
            offset.filter(|value| *value < max)
        }

        #[must_use]
        fn clamp_selection_range(&self, range: Option<(u32, u32)>) -> Option<(u32, u32)> {
            let max = self.grid_cell_capacity();
            let (a, b) = range?;
            let start = a.min(max);
            let end = b.min(max);
            if start == end {
                return None;
            }
            Some((start.min(end), start.max(end)))
        }

        fn sync_canvas_size(&mut self, geometry: GridGeometry) {
            let width = geometry.pixel_width.max(1);
            let height = geometry.pixel_height.max(1);
            if self.canvas.width() != width {
                self.canvas.set_width(width);
            }
            if self.canvas.height() != height {
                self.canvas.set_height(height);
            }
        }

        fn set_fill_style(&self, color: RgbaColor) {
            self.context.set_fill_style_str(&color.to_css_rgba());
        }

        #[must_use]
        fn dpr(&self) -> f32 {
            self.dpr
        }

        #[must_use]
        fn zoom(&self) -> f32 {
            self.zoom
        }

        fn resize(&mut self, cols: u16, rows: u16) {
            if cols == self.cols && rows == self.rows {
                return;
            }
            self.cols = cols;
            self.rows = rows;
            self.cells_cpu
                .resize(usize::from(cols) * usize::from(rows), CellData::EMPTY);
            self.cursor_offset = self.clamp_cursor_offset(self.cursor_offset);
            if self.cursor_offset.is_none() {
                self.cursor_style = CursorStyle::None;
            }
            self.selection_range = self.clamp_selection_range(self.selection_range);
            self.sync_canvas_size(self.current_geometry());
            self.apply_context_defaults();
        }

        fn set_scale(&mut self, dpr: f32, zoom: f32) {
            self.dpr = normalized_scale(dpr, 1.0, MIN_DPR, MAX_DPR);
            self.zoom = normalized_scale(zoom, 1.0, MIN_ZOOM, MAX_ZOOM);
            self.sync_canvas_size(self.current_geometry());
            self.apply_context_defaults();
        }

        #[must_use]
        fn current_geometry(&self) -> GridGeometry {
            grid_geometry(
                self.cols,
                self.rows,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            )
        }

        fn fit_to_container(
            &mut self,
            container_width_css: u32,
            container_height_css: u32,
        ) -> GridGeometry {
            let fit = fit_grid_to_container(
                container_width_css,
                container_height_css,
                self.cell_width,
                self.cell_height,
                self.dpr,
                self.zoom,
            );
            self.resize(fit.cols, fit.rows);
            fit
        }

        fn set_hovered_link_id(&mut self, link_id: u32) {
            self.hovered_link_id = link_id;
        }

        fn set_cursor(&mut self, offset: Option<u32>, style: CursorStyle) {
            self.cursor_offset = self.clamp_cursor_offset(offset);
            self.cursor_style = if self.cursor_offset.is_some() {
                style
            } else {
                CursorStyle::None
            };
        }

        fn set_selection_range(&mut self, range: Option<(u32, u32)>) {
            self.selection_range = self.clamp_selection_range(range);
        }

        fn apply_patches(&mut self, patches: &[CellPatch]) -> u32 {
            let max = self.grid_cell_capacity();
            let mut dirty = 0u32;

            for patch in patches {
                if patch.offset >= max {
                    continue;
                }
                let start = patch.offset as usize;
                let available = (max as usize).saturating_sub(start);
                let count = patch.cells.len().min(available);
                if count == 0 {
                    continue;
                }
                let end = start.saturating_add(count).min(self.cells_cpu.len());
                let actual_count = end.saturating_sub(start);
                if actual_count == 0 {
                    continue;
                }
                self.cells_cpu[start..end].copy_from_slice(&patch.cells[..actual_count]);
                dirty = dirty.saturating_add(actual_count as u32);
            }

            self.last_dirty_cells = dirty;
            dirty
        }

        fn render_frame(&mut self) -> Result<FrameStats, RendererError> {
            let geometry = self.current_geometry();
            self.sync_canvas_size(geometry);
            self.apply_context_defaults();

            let pixel_width = f64::from(geometry.pixel_width);
            let pixel_height = f64::from(geometry.pixel_height);
            self.context.set_fill_style_str("rgba(0, 0, 0, 1.0)");
            self.context.fill_rect(0.0, 0.0, pixel_width, pixel_height);

            let cols = u32::from(self.cols.max(1));
            let cell_width = f64::from(geometry.cell_width_px);
            let cell_height = f64::from(geometry.cell_height_px);
            let underline_height = (cell_height * 0.08).max(1.0);
            let strike_height = (cell_height * 0.08).max(1.0);
            let cursor_bar_width = (cell_width * 0.12).max(1.0);
            let selection = self.selection_range;
            let hovered_link_id = self.hovered_link_id;

            let font_px = (cell_height * 0.82).max(1.0);
            let regular_font = format!("{font_px:.2}px monospace");
            let bold_font = format!("bold {font_px:.2}px monospace");
            let italic_font = format!("italic {font_px:.2}px monospace");
            let bold_italic_font = format!("italic bold {font_px:.2}px monospace");

            let mut glyph_buf = [0u8; 4];
            for (index, cell) in self.cells_cpu.iter().enumerate() {
                let offset = index as u32;
                let col = offset % cols;
                let row = offset / cols;
                let x = f64::from(col) * cell_width;
                let y = f64::from(row) * cell_height;

                let style = cell_attr_style_bits(cell.attrs);
                let link_id = cell_attr_link_id(cell.attrs);
                let mut bg = RgbaColor::from_packed(cell.bg_rgba);
                let mut fg = RgbaColor::from_packed(cell.fg_rgba);

                if (style & ATTR_REVERSE) != 0 {
                    std::mem::swap(&mut bg, &mut fg);
                }
                let selected = selection
                    .map(|(start, end)| start < end && offset >= start && offset < end)
                    .unwrap_or(false);
                if selected {
                    std::mem::swap(&mut bg, &mut fg);
                }

                let cursor_here = self.cursor_offset == Some(offset);
                if cursor_here && self.cursor_style == CursorStyle::Block {
                    std::mem::swap(&mut bg, &mut fg);
                }

                if (style & ATTR_DIM) != 0 {
                    fg = fg.scale_rgb(0.6);
                }
                if (style & ATTR_BOLD) != 0 {
                    fg = fg.scale_rgb(1.2);
                }
                if (style & ATTR_BLINK) != 0 {
                    fg = fg.scale_alpha(0.85);
                }
                if (style & ATTR_HIDDEN) != 0 {
                    fg = fg.with_alpha(0.0);
                }

                self.set_fill_style(bg);
                self.context.fill_rect(x, y, cell_width, cell_height);

                let italic = (style & ATTR_ITALIC) != 0;
                let bold = (style & ATTR_BOLD) != 0;
                if cell.glyph_id != 0 && fg.a > 0 {
                    let glyph = char::from_u32(cell.glyph_id).unwrap_or('□');
                    if !glyph.is_whitespace() {
                        if bold && italic {
                            self.context.set_font(&bold_italic_font);
                        } else if bold {
                            self.context.set_font(&bold_font);
                        } else if italic {
                            self.context.set_font(&italic_font);
                        } else {
                            self.context.set_font(&regular_font);
                        }

                        self.set_fill_style(fg);
                        let glyph_text = glyph.encode_utf8(&mut glyph_buf);
                        self.context.fill_text(glyph_text, x, y).map_err(|error| {
                            RendererError::SurfaceError(format!(
                                "canvas2d fill_text failed: {error:?}"
                            ))
                        })?;
                    }
                }

                let underline = (style & ATTR_UNDERLINE) != 0;
                let strike = (style & ATTR_STRIKETHROUGH) != 0;
                let hover_underline = hovered_link_id != 0 && link_id == hovered_link_id;
                let cursor_bar = cursor_here && self.cursor_style == CursorStyle::Bar;
                let cursor_underline = cursor_here && self.cursor_style == CursorStyle::Underline;

                if underline || hover_underline || cursor_underline {
                    self.set_fill_style(fg);
                    self.context.fill_rect(
                        x,
                        y + (cell_height - underline_height),
                        cell_width,
                        underline_height,
                    );
                }

                if strike {
                    self.set_fill_style(fg);
                    self.context
                        .fill_rect(x, y + (cell_height * 0.55), cell_width, strike_height);
                }

                if cursor_bar {
                    self.set_fill_style(fg);
                    self.context.fill_rect(x, y, cursor_bar_width, cell_height);
                }
            }

            let dirty_cells = self.last_dirty_cells;
            self.last_dirty_cells = 0;
            Ok(FrameStats {
                instance_count: self.grid_cell_capacity(),
                dirty_cells,
            })
        }

        #[must_use]
        fn grid_size(&self) -> (u16, u16) {
            (self.cols, self.rows)
        }
    }

    enum RendererBackend {
        WebGpu(Box<GpuRenderer>),
        Canvas2d(Box<Canvas2dRenderer>),
    }

    /// Terminal renderer that prefers WebGPU and falls back to Canvas2D.
    pub struct WebGpuRenderer {
        backend: RendererBackend,
    }

    impl WebGpuRenderer {
        /// Initialize the renderer on the given canvas.
        ///
        /// First tries WebGPU; if adapter/device/surface setup fails, falls
        /// back to a Canvas2D implementation to keep demos functional.
        pub async fn init(
            canvas: HtmlCanvasElement,
            cols: u16,
            rows: u16,
            config: &RendererConfig,
        ) -> Result<Self, RendererError> {
            match GpuRenderer::init(canvas.clone(), cols, rows, config).await {
                Ok(renderer) => Ok(Self {
                    backend: RendererBackend::WebGpu(Box::new(renderer)),
                }),
                Err(webgpu_error) => Canvas2dRenderer::init(canvas, cols, rows, config)
                    .map(|renderer| Self {
                        backend: RendererBackend::Canvas2d(Box::new(renderer)),
                    })
                    .map_err(|canvas_error| {
                        RendererError::SurfaceError(format!(
                            "WebGPU init failed ({webgpu_error}); Canvas2D fallback init failed ({canvas_error})"
                        ))
                    }),
            }
        }

        /// Resize the grid.
        pub fn resize(&mut self, cols: u16, rows: u16) {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.resize(cols, rows),
                RendererBackend::Canvas2d(renderer) => renderer.resize(cols, rows),
            }
        }

        #[must_use]
        pub fn dpr(&self) -> f32 {
            match &self.backend {
                RendererBackend::WebGpu(renderer) => renderer.dpr(),
                RendererBackend::Canvas2d(renderer) => renderer.dpr(),
            }
        }

        #[must_use]
        pub fn zoom(&self) -> f32 {
            match &self.backend {
                RendererBackend::WebGpu(renderer) => renderer.zoom(),
                RendererBackend::Canvas2d(renderer) => renderer.zoom(),
            }
        }

        /// Update DPR/zoom while preserving current grid dimensions.
        pub fn set_scale(&mut self, dpr: f32, zoom: f32) {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.set_scale(dpr, zoom),
                RendererBackend::Canvas2d(renderer) => renderer.set_scale(dpr, zoom),
            }
        }

        #[must_use]
        pub fn current_geometry(&self) -> GridGeometry {
            match &self.backend {
                RendererBackend::WebGpu(renderer) => renderer.current_geometry(),
                RendererBackend::Canvas2d(renderer) => renderer.current_geometry(),
            }
        }

        /// Fit the grid to a CSS-pixel container and resize the renderer.
        pub fn fit_to_container(
            &mut self,
            container_width_css: u32,
            container_height_css: u32,
        ) -> GridGeometry {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => {
                    renderer.fit_to_container(container_width_css, container_height_css)
                }
                RendererBackend::Canvas2d(renderer) => {
                    renderer.fit_to_container(container_width_css, container_height_css)
                }
            }
        }

        /// Set currently hovered hyperlink ID (0 clears hover underline).
        pub fn set_hovered_link_id(&mut self, link_id: u32) {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.set_hovered_link_id(link_id),
                RendererBackend::Canvas2d(renderer) => renderer.set_hovered_link_id(link_id),
            }
        }

        /// Configure cursor overlay state.
        pub fn set_cursor(&mut self, offset: Option<u32>, style: CursorStyle) {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.set_cursor(offset, style),
                RendererBackend::Canvas2d(renderer) => renderer.set_cursor(offset, style),
            }
        }

        /// Configure selection overlay as a `[start, end)` offset range.
        pub fn set_selection_range(&mut self, range: Option<(u32, u32)>) {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.set_selection_range(range),
                RendererBackend::Canvas2d(renderer) => renderer.set_selection_range(range),
            }
        }

        /// Apply dirty-span cell patches.
        pub fn apply_patches(&mut self, patches: &[CellPatch]) -> u32 {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.apply_patches(patches),
                RendererBackend::Canvas2d(renderer) => renderer.apply_patches(patches),
            }
        }

        /// Render one frame.
        pub fn render_frame(&mut self) -> Result<FrameStats, RendererError> {
            match &mut self.backend {
                RendererBackend::WebGpu(renderer) => renderer.render_frame(),
                RendererBackend::Canvas2d(renderer) => renderer.render_frame(),
            }
        }

        /// Current grid dimensions.
        #[must_use]
        pub fn grid_size(&self) -> (u16, u16) {
            match &self.backend {
                RendererBackend::WebGpu(renderer) => renderer.grid_size(),
                RendererBackend::Canvas2d(renderer) => renderer.grid_size(),
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use gpu::WebGpuRenderer;

// ---------------------------------------------------------------------------
// Helpers (used by the wasm32-only gpu module and tests)
// ---------------------------------------------------------------------------

#[cfg(any(target_arch = "wasm32", test))]
fn cells_to_bytes(cells: &[CellData]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(cells.len() * CELL_DATA_BYTES);
    for cell in cells {
        bytes.extend_from_slice(&cell.to_bytes());
    }
    bytes
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InteractionUniforms {
    hovered_link_id: u32,
    cursor_offset: u32,
    cursor_style: u32,
    selection_active: u32,
    selection_start: u32,
    selection_end: u32,
}

#[cfg(any(target_arch = "wasm32", test))]
fn uniforms_bytes(
    viewport_w: f32,
    viewport_h: f32,
    cell_w: f32,
    cell_h: f32,
    cols: u32,
    rows: u32,
    interaction: InteractionUniforms,
) -> [u8; UNIFORM_BYTES] {
    let mut buf = [0u8; UNIFORM_BYTES];
    buf[0..4].copy_from_slice(&viewport_w.to_le_bytes());
    buf[4..8].copy_from_slice(&viewport_h.to_le_bytes());
    buf[8..12].copy_from_slice(&cell_w.to_le_bytes());
    buf[12..16].copy_from_slice(&cell_h.to_le_bytes());
    buf[16..20].copy_from_slice(&cols.to_le_bytes());
    buf[20..24].copy_from_slice(&rows.to_le_bytes());
    buf[32..36].copy_from_slice(&interaction.hovered_link_id.to_le_bytes());
    buf[36..40].copy_from_slice(&interaction.cursor_offset.to_le_bytes());
    buf[40..44].copy_from_slice(&interaction.cursor_style.to_le_bytes());
    buf[44..48].copy_from_slice(&interaction.selection_active.to_le_bytes());
    buf[48..52].copy_from_slice(&interaction.selection_start.to_le_bytes());
    buf[52..56].copy_from_slice(&interaction.selection_end.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph_atlas::{AtlasRect, GlyphMetrics};

    fn read_u32(bytes: &[u8]) -> u32 {
        u32::from_le_bytes(
            bytes
                .try_into()
                .expect("test slice must contain exactly 4 bytes"),
        )
    }

    fn read_f32(bytes: &[u8]) -> f32 {
        f32::from_le_bytes(
            bytes
                .try_into()
                .expect("test slice must contain exactly 4 bytes"),
        )
    }

    #[test]
    fn cell_data_to_bytes_roundtrip() {
        let cell = CellData {
            bg_rgba: 0xFF00FF80,
            fg_rgba: 0x00FF00FF,
            glyph_id: 42,
            attrs: 0b0000_0111,
        };
        let bytes = cell.to_bytes();
        assert_eq!(bytes.len(), CELL_DATA_BYTES);
        assert_eq!(read_u32(&bytes[0..4]), 0xFF00FF80);
        assert_eq!(read_u32(&bytes[4..8]), 0x00FF00FF);
        assert_eq!(read_u32(&bytes[8..12]), 42);
        assert_eq!(read_u32(&bytes[12..16]), 7);
    }

    #[test]
    fn packed_attrs_decode_style_and_link() {
        let attrs = 0x00AB_CDEFu32;
        assert_eq!(cell_attr_style_bits(attrs), 0xEF);
        assert_eq!(cell_attr_link_id(attrs), 0x0000_ABCD);
    }

    #[test]
    fn cursor_style_roundtrip() {
        assert_eq!(CursorStyle::from_u32(0), CursorStyle::None);
        assert_eq!(CursorStyle::from_u32(1), CursorStyle::Block);
        assert_eq!(CursorStyle::from_u32(2), CursorStyle::Bar);
        assert_eq!(CursorStyle::from_u32(3), CursorStyle::Underline);
        assert_eq!(CursorStyle::from_u32(99), CursorStyle::None);
    }

    #[test]
    fn cells_to_bytes_length() {
        let cells = vec![CellData::EMPTY; 10];
        let bytes = cells_to_bytes(&cells);
        assert_eq!(bytes.len(), 10 * CELL_DATA_BYTES);
    }

    #[test]
    fn uniforms_bytes_layout() {
        let buf = uniforms_bytes(
            800.0,
            600.0,
            8.0,
            16.0,
            100,
            37,
            InteractionUniforms {
                hovered_link_id: 123,
                cursor_offset: 456,
                cursor_style: CursorStyle::Bar.as_u32(),
                selection_active: 1,
                selection_start: 40,
                selection_end: 88,
            },
        );
        assert_eq!(buf.len(), UNIFORM_BYTES);
        let vw = read_f32(&buf[0..4]);
        let vh = read_f32(&buf[4..8]);
        let cw = read_f32(&buf[8..12]);
        let ch = read_f32(&buf[12..16]);
        let cols = read_u32(&buf[16..20]);
        let rows = read_u32(&buf[20..24]);
        let hovered_link_id = read_u32(&buf[32..36]);
        let cursor_offset = read_u32(&buf[36..40]);
        let cursor_style = read_u32(&buf[40..44]);
        let selection_active = read_u32(&buf[44..48]);
        let selection_start = read_u32(&buf[48..52]);
        let selection_end = read_u32(&buf[52..56]);
        assert_eq!(vw, 800.0);
        assert_eq!(vh, 600.0);
        assert_eq!(cw, 8.0);
        assert_eq!(ch, 16.0);
        assert_eq!(cols, 100);
        assert_eq!(rows, 37);
        assert_eq!(hovered_link_id, 123);
        assert_eq!(cursor_offset, 456);
        assert_eq!(cursor_style, CursorStyle::Bar.as_u32());
        assert_eq!(selection_active, 1);
        assert_eq!(selection_start, 40);
        assert_eq!(selection_end, 88);
    }

    #[test]
    fn cell_data_default_is_empty() {
        let d = CellData::default();
        assert_eq!(d, CellData::EMPTY);
        assert_eq!(d.bg_rgba, 0x000000FF);
        assert_eq!(d.fg_rgba, 0xFFFFFFFF);
    }

    #[test]
    fn grid_geometry_is_deterministic_for_fractional_dpr() {
        let a = grid_geometry(80, 24, 8, 16, 1.25, 1.0);
        let b = grid_geometry(80, 24, 8, 16, 1.25, 1.0);
        assert_eq!(a, b);
        assert_eq!(a.pixel_width, 800);
        assert_eq!(a.pixel_height, 480);
        assert_eq!(a.cell_width_px, 10.0);
        assert_eq!(a.cell_height_px, 20.0);
    }

    #[test]
    fn fit_grid_to_container_never_exceeds_pixel_container() {
        let g = fit_grid_to_container(803, 611, 8, 16, 1.0, 1.0);
        assert!(g.pixel_width <= 803);
        assert!(g.pixel_height <= 611);
        assert!(g.cols >= 1);
        assert!(g.rows >= 1);
    }

    #[test]
    fn fit_grid_to_container_zoom_reduces_capacity() {
        let base = fit_grid_to_container(800, 600, 8, 16, 1.0, 1.0);
        let zoomed = fit_grid_to_container(800, 600, 8, 16, 1.0, 1.75);
        assert!(zoomed.cols < base.cols);
        assert!(zoomed.rows < base.rows);
    }

    #[test]
    fn geometry_normalizes_invalid_scales() {
        let g = grid_geometry(80, 24, 8, 16, f32::NAN, -2.0);
        assert_eq!(g.dpr, 1.0);
        assert_eq!(g.zoom, 1.0);
    }

    #[test]
    fn glyph_meta_entry_from_placement_encodes_uv_and_layout() {
        let placement = GlyphPlacement {
            id: 1,
            slot: AtlasRect {
                x: 8,
                y: 8,
                w: 16,
                h: 16,
            },
            draw: AtlasRect {
                x: 10,
                y: 20,
                w: 8,
                h: 12,
            },
            metrics: GlyphMetrics {
                advance_x: 8,
                bearing_x: 2,
                bearing_y: 12,
            },
        };
        let meta = GlyphMetaEntry::from_placement(placement, 100, 200, 8, 16);
        assert!((meta.uv_min_x - 0.10).abs() < f32::EPSILON);
        assert!((meta.uv_min_y - 0.10).abs() < f32::EPSILON);
        assert!((meta.uv_max_x - 0.18).abs() < f32::EPSILON);
        assert!((meta.uv_max_y - 0.16).abs() < f32::EPSILON);
        assert!((meta.layout_min_x - 0.25).abs() < f32::EPSILON);
        assert!((meta.layout_min_y - 0.25).abs() < f32::EPSILON);
        assert!((meta.layout_max_x - 1.25).abs() < f32::EPSILON);
        assert!((meta.layout_max_y - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn glyph_meta_entry_to_bytes_includes_layout_fields() {
        let meta = GlyphMetaEntry {
            uv_min_x: 0.1,
            uv_min_y: 0.2,
            uv_max_x: 0.3,
            uv_max_y: 0.4,
            layout_min_x: -0.1,
            layout_min_y: 0.0,
            layout_max_x: 1.2,
            layout_max_y: 1.1,
        };
        let bytes = meta.to_bytes();
        assert_eq!(bytes.len(), GLYPH_META_BYTES);
        assert_eq!(read_f32(&bytes[16..20]), -0.1);
        assert_eq!(read_f32(&bytes[20..24]), 0.0);
        assert_eq!(read_f32(&bytes[24..28]), 1.2);
        assert_eq!(read_f32(&bytes[28..32]), 1.1);
    }

    // ── normalized_scale edge cases ────────────────────────────────────

    #[test]
    fn normalized_scale_clamps_below_min() {
        assert_eq!(normalized_scale(0.1, 1.0, 0.25, 8.0), 0.25);
    }

    #[test]
    fn normalized_scale_clamps_above_max() {
        assert_eq!(normalized_scale(100.0, 1.0, 0.25, 8.0), 8.0);
    }

    #[test]
    fn normalized_scale_nan_returns_fallback() {
        assert_eq!(normalized_scale(f32::NAN, 1.0, 0.25, 8.0), 1.0);
    }

    #[test]
    fn normalized_scale_infinity_returns_fallback() {
        assert_eq!(normalized_scale(f32::INFINITY, 1.0, 0.25, 8.0), 1.0);
    }

    #[test]
    fn normalized_scale_neg_infinity_returns_fallback() {
        assert_eq!(normalized_scale(f32::NEG_INFINITY, 1.0, 0.25, 8.0), 1.0);
    }

    #[test]
    fn normalized_scale_zero_returns_fallback() {
        assert_eq!(normalized_scale(0.0, 1.0, 0.25, 8.0), 1.0);
    }

    #[test]
    fn normalized_scale_negative_returns_fallback() {
        assert_eq!(normalized_scale(-1.0, 1.0, 0.25, 8.0), 1.0);
    }

    #[test]
    fn normalized_scale_at_min_boundary() {
        assert_eq!(normalized_scale(0.25, 1.0, 0.25, 8.0), 0.25);
    }

    #[test]
    fn normalized_scale_at_max_boundary() {
        assert_eq!(normalized_scale(8.0, 1.0, 0.25, 8.0), 8.0);
    }

    // ── grid_geometry edge cases ───────────────────────────────────────

    #[test]
    fn grid_geometry_zero_cols_yields_zero_width() {
        let g = grid_geometry(0, 24, 8, 16, 1.0, 1.0);
        assert_eq!(g.cols, 0);
        assert_eq!(g.pixel_width, 0);
        assert!(g.pixel_height > 0);
    }

    #[test]
    fn grid_geometry_zero_rows_yields_zero_height() {
        let g = grid_geometry(80, 0, 8, 16, 1.0, 1.0);
        assert_eq!(g.rows, 0);
        assert!(g.pixel_width > 0);
        assert_eq!(g.pixel_height, 0);
    }

    #[test]
    fn grid_geometry_zero_cell_width_clamps_to_one() {
        let g = grid_geometry(80, 24, 0, 16, 1.0, 1.0);
        assert!(g.cell_width_px >= 1.0);
        assert!(g.pixel_width >= 80);
    }

    #[test]
    fn grid_geometry_zero_cell_height_clamps_to_one() {
        let g = grid_geometry(80, 24, 8, 0, 1.0, 1.0);
        assert!(g.cell_height_px >= 1.0);
        assert!(g.pixel_height >= 24);
    }

    #[test]
    fn grid_geometry_high_dpr_scales_pixel_dimensions() {
        let g1 = grid_geometry(80, 24, 8, 16, 1.0, 1.0);
        let g2 = grid_geometry(80, 24, 8, 16, 2.0, 1.0);
        assert!(g2.pixel_width > g1.pixel_width);
        assert!(g2.pixel_height > g1.pixel_height);
    }

    #[test]
    fn grid_geometry_nan_dpr_treated_as_1() {
        let g = grid_geometry(80, 24, 8, 16, f32::NAN, 1.0);
        assert_eq!(g.dpr, 1.0);
        assert_eq!(g.pixel_width, 640);
        assert_eq!(g.pixel_height, 384);
    }

    #[test]
    fn grid_geometry_nan_zoom_treated_as_1() {
        let g = grid_geometry(80, 24, 8, 16, 1.0, f32::NAN);
        assert_eq!(g.zoom, 1.0);
    }

    #[test]
    fn grid_geometry_extreme_dpr_clamped() {
        let g = grid_geometry(80, 24, 8, 16, 100.0, 1.0);
        assert_eq!(g.dpr, 8.0); // MAX_DPR
    }

    #[test]
    fn grid_geometry_extreme_zoom_clamped() {
        let g = grid_geometry(80, 24, 8, 16, 1.0, 100.0);
        assert_eq!(g.zoom, 4.0); // MAX_ZOOM
    }

    // ── fit_grid_to_container edge cases ───────────────────────────────

    #[test]
    fn fit_grid_to_container_tiny_container_yields_1x1() {
        let g = fit_grid_to_container(1, 1, 8, 16, 1.0, 1.0);
        assert_eq!(g.cols, 1);
        assert_eq!(g.rows, 1);
    }

    #[test]
    fn fit_grid_to_container_exact_multiple() {
        let g = fit_grid_to_container(640, 384, 8, 16, 1.0, 1.0);
        assert_eq!(g.cols, 80);
        assert_eq!(g.rows, 24);
        assert_eq!(g.pixel_width, 640);
        assert_eq!(g.pixel_height, 384);
    }

    #[test]
    fn fit_grid_to_container_nan_dpr_uses_fallback() {
        let g = fit_grid_to_container(800, 600, 8, 16, f32::NAN, 1.0);
        assert_eq!(g.dpr, 1.0);
        assert!(g.cols >= 1);
        assert!(g.rows >= 1);
    }

    #[test]
    fn fit_grid_to_container_high_zoom_reduces_grid() {
        let base = fit_grid_to_container(800, 600, 8, 16, 1.0, 1.0);
        let max_zoom = fit_grid_to_container(800, 600, 8, 16, 1.0, 4.0);
        assert!(max_zoom.cols < base.cols);
        assert!(max_zoom.rows < base.rows);
    }

    // ── RendererConfig defaults ────────────────────────────────────────

    #[test]
    fn renderer_config_defaults() {
        let cfg = RendererConfig::default();
        assert_eq!(cfg.cell_width, 8);
        assert_eq!(cfg.cell_height, 16);
        assert_eq!(cfg.dpr, 1.0);
        assert_eq!(cfg.zoom, 1.0);
    }

    // ── RendererError Display ──────────────────────────────────────────

    #[test]
    fn renderer_error_display_no_adapter() {
        let err = RendererError::NoAdapter;
        assert_eq!(err.to_string(), "WebGPU adapter not available");
    }

    #[test]
    fn renderer_error_display_device_error() {
        let err = RendererError::DeviceError("out of memory".into());
        assert_eq!(err.to_string(), "WebGPU device error: out of memory");
    }

    #[test]
    fn renderer_error_display_surface_error() {
        let err = RendererError::SurfaceError("lost".into());
        assert_eq!(err.to_string(), "WebGPU surface error: lost");
    }

    #[test]
    fn renderer_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(RendererError::DeviceError("test".into()));
        assert!(err.to_string().contains("test"));
    }

    // ── CellData attribute encoding ────────────────────────────────────

    #[test]
    fn cell_attr_style_bits_extracts_low_byte() {
        assert_eq!(cell_attr_style_bits(0x0000_00FF), 0xFF);
        assert_eq!(cell_attr_style_bits(0xFFFF_FF00), 0x00);
        assert_eq!(cell_attr_style_bits(0x1234_5678), 0x78);
    }

    #[test]
    fn cell_attr_link_id_extracts_shifted_upper() {
        assert_eq!(cell_attr_link_id(0x00AB_CD00), 0x0000_ABCD);
        assert_eq!(cell_attr_link_id(0x0000_00FF), 0x0000_0000);
        assert_eq!(cell_attr_link_id(0xFFFF_FF00), 0x00FF_FFFF);
    }

    #[test]
    fn cell_data_empty_has_expected_defaults() {
        let empty = CellData::EMPTY;
        assert_eq!(empty.glyph_id, 0);
        assert_eq!(empty.attrs, 0);
        assert_eq!(cell_attr_style_bits(empty.attrs), 0);
        assert_eq!(cell_attr_link_id(empty.attrs), 0);
    }

    // ── CellPatch / cells_to_bytes ─────────────────────────────────────

    #[test]
    fn cells_to_bytes_empty_slice() {
        let bytes = cells_to_bytes(&[]);
        assert!(bytes.is_empty());
    }

    #[test]
    fn cell_patch_with_single_cell() {
        let patch = CellPatch {
            offset: 42,
            cells: vec![CellData {
                bg_rgba: 0xAABBCCDD,
                fg_rgba: 0x11223344,
                glyph_id: 7,
                attrs: 0,
            }],
        };
        assert_eq!(patch.offset, 42);
        assert_eq!(patch.cells.len(), 1);
        let bytes = cells_to_bytes(&patch.cells);
        assert_eq!(bytes.len(), CELL_DATA_BYTES);
    }

    #[test]
    fn cell_patch_multi_cell_byte_length() {
        let patch = CellPatch {
            offset: 0,
            cells: vec![CellData::EMPTY; 5],
        };
        let bytes = cells_to_bytes(&patch.cells);
        assert_eq!(bytes.len(), 5 * CELL_DATA_BYTES);
    }

    #[test]
    fn cell_patch_from_flat_u32_decodes_cell_words() {
        let patch = cell_patch_from_flat_u32(
            7,
            &[
                0x0102_0304,
                0xA0B0_C0D0,
                0x0000_03B1,
                0x0012_3401,
                0xDEAD_BEEF,
                0x1234_5678,
                0x0001_F642,
                0x00AB_CDFF,
            ],
        )
        .expect("flat payload should decode");

        assert_eq!(patch.offset, 7);
        assert_eq!(patch.cells.len(), 2);
        assert_eq!(
            patch.cells[0],
            CellData {
                bg_rgba: 0x0102_0304,
                fg_rgba: 0xA0B0_C0D0,
                glyph_id: 0x0000_03B1,
                attrs: 0x0012_3401,
            }
        );
        assert_eq!(
            patch.cells[1],
            CellData {
                bg_rgba: 0xDEAD_BEEF,
                fg_rgba: 0x1234_5678,
                glyph_id: 0x0001_F642,
                attrs: 0x00AB_CDFF,
            }
        );
    }

    #[test]
    fn cell_patch_from_flat_u32_rejects_incomplete_tuple() {
        let err = cell_patch_from_flat_u32(0, &[1, 2, 3]).expect_err("invalid length must fail");
        assert_eq!(err, "flat cell payload length must be a multiple of 4");
    }

    #[test]
    fn cell_patches_from_flat_u32_decodes_multiple_spans() {
        let patches = cell_patches_from_flat_u32(
            &[2, 1, 8, 2],
            &[
                1, 2, 3, 4, //
                5, 6, 7, 8, //
                9, 10, 11, 12,
            ],
        )
        .expect("valid flat batch should decode");

        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].offset, 2);
        assert_eq!(patches[0].cells.len(), 1);
        assert_eq!(patches[1].offset, 8);
        assert_eq!(patches[1].cells.len(), 2);
        assert_eq!(patches[1].cells[1].glyph_id, 11);
    }

    #[test]
    fn cell_patches_from_flat_u32_rejects_mismatched_span_lengths() {
        let err = cell_patches_from_flat_u32(
            &[0, 2],
            &[
                1, 2, 3, 4, //
                5, 6, 7, 8, //
                9, 10, 11, 12,
            ],
        )
        .expect_err("extra payload words must fail");
        assert_eq!(err, "flat spans do not consume entire cell payload");
    }

    #[test]
    fn cell_patches_from_flat_u32_rejects_odd_span_payload() {
        let err = cell_patches_from_flat_u32(&[0, 1, 2], &[1, 2, 3, 4])
            .expect_err("odd spans length must fail");
        assert_eq!(err, "flat span payload length must be even");
    }

    // ── CursorStyle exhaustive ─────────────────────────────────────────

    #[test]
    fn cursor_style_roundtrip_all_variants() {
        for val in 0..=3 {
            let style = CursorStyle::from_u32(val);
            assert_eq!(style.as_u32(), val);
        }
    }

    #[test]
    fn cursor_style_out_of_range_maps_to_none() {
        for val in [4, 100, u32::MAX] {
            assert_eq!(CursorStyle::from_u32(val), CursorStyle::None);
        }
    }

    // ── InteractionUniforms in uniform buffer ──────────────────────────

    #[test]
    fn uniforms_bytes_zero_interaction_state() {
        let buf = uniforms_bytes(
            100.0,
            100.0,
            8.0,
            16.0,
            10,
            10,
            InteractionUniforms {
                hovered_link_id: 0,
                cursor_offset: 0,
                cursor_style: CursorStyle::None.as_u32(),
                selection_active: 0,
                selection_start: 0,
                selection_end: 0,
            },
        );
        assert_eq!(buf.len(), UNIFORM_BYTES);
        // Interaction fields are at offset 32..56
        for i in (32..56).step_by(4) {
            assert_eq!(
                read_u32(&buf[i..i + 4]),
                0,
                "interaction uniform at offset {i} should be zero"
            );
        }
    }

    #[test]
    fn uniforms_bytes_preserves_viewport_and_grid() {
        let buf = uniforms_bytes(
            1920.0,
            1080.0,
            10.0,
            20.0,
            192,
            54,
            InteractionUniforms {
                hovered_link_id: 0,
                cursor_offset: 0,
                cursor_style: 0,
                selection_active: 0,
                selection_start: 0,
                selection_end: 0,
            },
        );
        assert_eq!(read_f32(&buf[0..4]), 1920.0);
        assert_eq!(read_f32(&buf[4..8]), 1080.0);
        assert_eq!(read_f32(&buf[8..12]), 10.0);
        assert_eq!(read_f32(&buf[12..16]), 20.0);
        assert_eq!(read_u32(&buf[16..20]), 192);
        assert_eq!(read_u32(&buf[20..24]), 54);
    }

    // ── FrameStats ─────────────────────────────────────────────────────

    #[test]
    fn frame_stats_default_is_zero() {
        let stats = FrameStats::default();
        assert_eq!(stats.instance_count, 0);
        assert_eq!(stats.dirty_cells, 0);
    }
}
