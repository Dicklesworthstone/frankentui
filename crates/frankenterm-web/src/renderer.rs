//! WebGPU renderer skeleton for FrankenTerm.
//!
//! Implements the architecture from ADR-009: single-pass instanced cell quads
//! driven by a storage buffer of per-cell data. The renderer consumes cell
//! patches (dirty spans) and never reads the Grid directly.
//!
//! This skeleton covers:
//! - WebGPU device initialization + surface configuration
//! - Resize handling (surface reconfiguration + instance buffer growth)
//! - Per-cell background color rendering via instanced quads
//! - Dirty-span patch updates via `queue.write_buffer` slices
//!
//! Text rendering (glyph atlas sampling) is deferred to bd-lff4p.2.4.

use std::fmt;

// ---------------------------------------------------------------------------
// Platform-agnostic types (available on all targets for type checking)
// ---------------------------------------------------------------------------

/// Size of one cell's GPU data in bytes (4 × u32 = 16 bytes).
pub const CELL_DATA_BYTES: usize = 16;

/// Size of the uniform buffer in bytes (2 × vec4 = 32 bytes).
#[cfg(any(target_arch = "wasm32", test))]
const UNIFORM_BYTES: usize = 32;

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
    /// Packed attributes: bold(0), italic(1), underline(2), reverse(3),
    /// dim(4), strikethrough(5), blink(6). Bits 8..31 reserved.
    pub attrs: u32,
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

/// Configuration for renderer initialization.
#[derive(Debug, Clone)]
pub struct RendererConfig {
    /// Cell width in CSS pixels.
    pub cell_width: u16,
    /// Cell height in CSS pixels.
    pub cell_height: u16,
    /// Device pixel ratio (e.g. 2.0 for Retina).
    pub dpr: f32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            cell_width: 8,
            cell_height: 16,
            dpr: 1.0,
        }
    }
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
}

struct CellData {
    bg_rgba: u32,
    fg_rgba: u32,
    glyph_id: u32,
    attrs: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<storage, read> cells: array<CellData>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
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
    let bg = cell.bg_rgba;
    let r = f32((bg >> 24u) & 0xFFu) / 255.0;
    let g = f32((bg >> 16u) & 0xFFu) / 255.0;
    let b = f32((bg >> 8u) & 0xFFu) / 255.0;
    let a = f32(bg & 0xFFu) / 255.0;

    var out: VertexOutput;
    out.position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.color = vec4<f32>(r, g, b, a);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

// ---------------------------------------------------------------------------
// WebGPU implementation (wasm32 only)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod gpu {
    use super::*;
    use web_sys::HtmlCanvasElement;
    use wgpu;

    /// WebGPU renderer owning all GPU resources.
    ///
    /// Follows ADR-009: single pipeline, instanced cell quads, storage-buffer
    /// driven, patch-based updates.
    pub struct WebGpuRenderer {
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        surface_config: wgpu::SurfaceConfiguration,
        pipeline: wgpu::RenderPipeline,
        uniform_buffer: wgpu::Buffer,
        cell_buffer: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        bind_group_layout: wgpu::BindGroupLayout,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
        dpr: f32,
        /// Shadow copy of cell data for resize-time buffer rebuilds.
        cells_cpu: Vec<CellData>,
    }

    impl WebGpuRenderer {
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

            let surface = instance
                .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
                .map_err(|e| RendererError::SurfaceError(e.to_string()))?;

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .ok_or(RendererError::NoAdapter)?;

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("frankenterm"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .map_err(|e| RendererError::DeviceError(e.to_string()))?;

            let dpr = config.dpr;
            let pixel_width = (cols as f32 * config.cell_width as f32 * dpr) as u32;
            let pixel_height = (rows as f32 * config.cell_height as f32 * dpr) as u32;

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

            // Bind group layout: uniform + storage.
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("cell_bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
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
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cell_pl"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
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
                multiview: None,
                cache: None,
            });

            // Buffers.
            let cell_count = (cols as usize) * (rows as usize);
            let cells_cpu = vec![CellData::EMPTY; cell_count];
            let cell_bytes = cells_to_bytes(&cells_cpu);

            let cell_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cells"),
                size: cell_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&cell_buffer, 0, &cell_bytes);

            let uniform_bytes = uniforms_bytes(
                pixel_width as f32,
                pixel_height as f32,
                config.cell_width as f32 * dpr,
                config.cell_height as f32 * dpr,
                cols as u32,
                rows as u32,
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
                cols,
                rows,
                cell_width: config.cell_width,
                cell_height: config.cell_height,
                dpr,
                cells_cpu,
            })
        }

        /// Resize the grid. Reconfigures the surface and rebuilds the cell buffer.
        pub fn resize(&mut self, cols: u16, rows: u16) {
            if cols == self.cols && rows == self.rows {
                return;
            }
            self.cols = cols;
            self.rows = rows;

            let pixel_w = (cols as f32 * self.cell_width as f32 * self.dpr) as u32;
            let pixel_h = (rows as f32 * self.cell_height as f32 * self.dpr) as u32;

            self.surface_config.width = pixel_w.max(1);
            self.surface_config.height = pixel_h.max(1);
            self.surface.configure(&self.device, &self.surface_config);

            // Rebuild cell buffer for new grid size.
            let cell_count = (cols as usize) * (rows as usize);
            self.cells_cpu.resize(cell_count, CellData::EMPTY);
            let cell_bytes = cells_to_bytes(&self.cells_cpu);

            self.cell_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cells"),
                size: cell_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&self.cell_buffer, 0, &cell_bytes);

            // Update uniforms.
            let ub = uniforms_bytes(
                pixel_w as f32,
                pixel_h as f32,
                self.cell_width as f32 * self.dpr,
                self.cell_height as f32 * self.dpr,
                cols as u32,
                rows as u32,
            );
            self.queue.write_buffer(&self.uniform_buffer, 0, &ub);

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
                ],
            });
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
                let cells = &patch.cells[..count];

                // Update CPU shadow.
                for (i, cell) in cells.iter().enumerate() {
                    self.cells_cpu[(start as usize) + i] = *cell;
                }

                // Upload only the dirty range to the GPU.
                let byte_offset = (start as u64) * (CELL_DATA_BYTES as u64);
                let bytes = cells_to_bytes(cells);
                self.queue
                    .write_buffer(&self.cell_buffer, byte_offset, &bytes);
                dirty += count as u32;
            }
            dirty
        }

        /// Encode and submit one render frame.
        pub fn render_frame(&self) -> Result<FrameStats, RendererError> {
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
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                // 6 vertices per cell (2 triangles), instanced per cell.
                pass.draw(0..6, 0..instance_count);
            }

            self.queue.submit(std::iter::once(encoder.finish()));
            output.present();

            Ok(FrameStats {
                instance_count,
                dirty_cells: 0,
            })
        }

        /// Current grid dimensions.
        #[must_use]
        pub fn grid_size(&self) -> (u16, u16) {
            (self.cols, self.rows)
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
fn uniforms_bytes(
    viewport_w: f32,
    viewport_h: f32,
    cell_w: f32,
    cell_h: f32,
    cols: u32,
    rows: u32,
) -> [u8; UNIFORM_BYTES] {
    let mut buf = [0u8; UNIFORM_BYTES];
    buf[0..4].copy_from_slice(&viewport_w.to_le_bytes());
    buf[4..8].copy_from_slice(&viewport_h.to_le_bytes());
    buf[8..12].copy_from_slice(&cell_w.to_le_bytes());
    buf[12..16].copy_from_slice(&cell_h.to_le_bytes());
    buf[16..20].copy_from_slice(&cols.to_le_bytes());
    buf[20..24].copy_from_slice(&rows.to_le_bytes());
    // bytes 24..32 are padding (zeroed).
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            0xFF00FF80
        );
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            0x00FF00FF
        );
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 42);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 7);
    }

    #[test]
    fn cells_to_bytes_length() {
        let cells = vec![CellData::EMPTY; 10];
        let bytes = cells_to_bytes(&cells);
        assert_eq!(bytes.len(), 10 * CELL_DATA_BYTES);
    }

    #[test]
    fn uniforms_bytes_layout() {
        let buf = uniforms_bytes(800.0, 600.0, 8.0, 16.0, 100, 37);
        assert_eq!(buf.len(), UNIFORM_BYTES);
        let vw = f32::from_le_bytes(buf[0..4].try_into().unwrap());
        let vh = f32::from_le_bytes(buf[4..8].try_into().unwrap());
        let cw = f32::from_le_bytes(buf[8..12].try_into().unwrap());
        let ch = f32::from_le_bytes(buf[12..16].try_into().unwrap());
        let cols = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let rows = u32::from_le_bytes(buf[20..24].try_into().unwrap());
        assert_eq!(vw, 800.0);
        assert_eq!(vh, 600.0);
        assert_eq!(cw, 8.0);
        assert_eq!(ch, 16.0);
        assert_eq!(cols, 100);
        assert_eq!(rows, 37);
    }

    #[test]
    fn cell_data_default_is_empty() {
        let d = CellData::default();
        assert_eq!(d, CellData::EMPTY);
        assert_eq!(d.bg_rgba, 0x000000FF);
        assert_eq!(d.fg_rgba, 0xFFFFFFFF);
    }
}
