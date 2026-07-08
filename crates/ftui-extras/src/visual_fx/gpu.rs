#![forbid(unsafe_code)]

//! Optional GPU acceleration for visual FX.
//!
//! This module is feature-gated behind `fx-gpu` and provides a minimal
//! compute pipeline for metaballs. It is designed to be failure-tolerant:
//! any init or render failure permanently disables GPU usage for the process.

use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::FxContext;
use ftui_render::cell::PackedRgba;

use bytemuck::{Pod, Zeroable};
use pollster::block_on;

const ENV_GPU_DISABLE: &str = "FTUI_FX_GPU_DISABLE";
const ENV_GPU_FORCE_FAIL: &str = "FTUI_FX_GPU_FORCE_FAIL";
const READBACK_TIMEOUT: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuDisableReason {
    ForcedByEnv,
    InitFailed,
    RenderFailed,
}

#[derive(Debug)]
#[allow(dead_code)]
enum GpuInitError {
    AdapterNotFound(wgpu::RequestAdapterError),
    RequestDevice(wgpu::RequestDeviceError),
}

#[derive(Debug)]
#[allow(dead_code)]
enum GpuState {
    Uninitialized,
    Available(GpuContext),
    Unavailable(GpuDisableReason),
}

#[derive(Debug)]
struct GpuBackend {
    state: GpuState,
}

impl GpuBackend {
    fn new() -> Self {
        Self {
            state: GpuState::Uninitialized,
        }
    }

    fn is_disabled(&self) -> bool {
        matches!(self.state, GpuState::Unavailable(_))
    }

    fn disable(&mut self, reason: GpuDisableReason) {
        self.state = GpuState::Unavailable(reason);
    }

    fn ensure_initialized(&mut self) -> Result<(), GpuDisableReason> {
        if matches!(self.state, GpuState::Available(_)) {
            return Ok(());
        }
        if matches!(self.state, GpuState::Unavailable(_)) {
            return Err(GpuDisableReason::InitFailed);
        }
        if env_truthy(ENV_GPU_FORCE_FAIL) {
            self.disable(GpuDisableReason::ForcedByEnv);
            return Err(GpuDisableReason::ForcedByEnv);
        }
        match GpuContext::new() {
            Ok(ctx) => {
                self.state = GpuState::Available(ctx);
                Ok(())
            }
            Err(_) => {
                self.disable(GpuDisableReason::InitFailed);
                Err(GpuDisableReason::InitFailed)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_metaballs(
        &mut self,
        ctx: FxContext<'_>,
        glow: f64,
        threshold: f64,
        bg_base: PackedRgba,
        stops: [PackedRgba; 4],
        balls: &[GpuBall],
        out: &mut [PackedRgba],
    ) -> Result<(), GpuDisableReason> {
        self.ensure_initialized()?;
        let state = std::mem::replace(&mut self.state, GpuState::Uninitialized);
        let mut ctx_state = match state {
            GpuState::Available(ctx_state) => ctx_state,
            other => {
                self.state = other;
                return Err(GpuDisableReason::InitFailed);
            }
        };

        let render_result =
            ctx_state.render_metaballs(ctx, glow, threshold, bg_base, stops, balls, out);
        self.state = GpuState::Available(ctx_state);
        if render_result.is_err() {
            self.disable(GpuDisableReason::RenderFailed);
            return Err(GpuDisableReason::RenderFailed);
        }
        Ok(())
    }
}

static GPU_BACKEND: OnceLock<Mutex<GpuBackend>> = OnceLock::new();

fn backend() -> &'static Mutex<GpuBackend> {
    GPU_BACKEND.get_or_init(|| Mutex::new(GpuBackend::new()))
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub(crate) fn gpu_enabled() -> bool {
    !env_truthy(ENV_GPU_DISABLE)
}

pub(crate) fn render_metaballs(
    ctx: FxContext<'_>,
    glow: f64,
    threshold: f64,
    bg_base: PackedRgba,
    stops: [PackedRgba; 4],
    balls: &[GpuBall],
    out: &mut [PackedRgba],
) -> bool {
    let mut guard = backend().lock().expect("gpu backend mutex poisoned");
    if guard.is_disabled() {
        return false;
    }
    if guard
        .render_metaballs(ctx, glow, threshold, bg_base, stops, balls, out)
        .is_ok()
    {
        return true;
    }
    false
}

/// Shared lock for all tests that mutate GPU global state.
///
/// Both `gpu::tests` and `metaballs::tests` manipulate the same `GPU_BACKEND`
/// singleton. This lock must be held by any test that calls `force_disable_for_tests`,
/// `force_init_fail_for_tests`, or `reset_for_tests` to prevent races.
#[cfg(test)]
pub(crate) fn gpu_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static GPU_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    GPU_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("gpu test lock poisoned")
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    if let Some(lock) = GPU_BACKEND.get() {
        let mut guard = lock.lock().expect("gpu backend mutex poisoned");
        guard.state = GpuState::Uninitialized;
    }
}

#[cfg(test)]
pub(crate) fn force_disable_for_tests() {
    let mut guard = backend().lock().expect("gpu backend mutex poisoned");
    guard.state = GpuState::Unavailable(GpuDisableReason::ForcedByEnv);
}

#[cfg(test)]
pub(crate) fn force_init_fail_for_tests() {
    let mut guard = backend().lock().expect("gpu backend mutex poisoned");
    guard.state = GpuState::Unavailable(GpuDisableReason::InitFailed);
}

#[cfg(test)]
pub(crate) fn is_disabled_for_tests() -> bool {
    GPU_BACKEND
        .get()
        .map(|lock| {
            lock.lock()
                .expect("gpu backend mutex poisoned")
                .is_disabled()
        })
        .unwrap_or(false)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub(crate) struct GpuBall {
    pub x: f32,
    pub y: f32,
    pub r2: f32,
    pub hue: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct MetaballsUniform {
    width: u32,
    height: u32,
    ball_count: u32,
    _pad0: u32,
    glow: f32,
    threshold: f32,
    _pad1: [f32; 2],
    bg_base: [f32; 4],
    stop0: [f32; 4],
    stop1: [f32; 4],
    stop2: [f32; 4],
    stop3: [f32; 4],
}

struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    balls_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    output_capacity: usize,
    balls_capacity: usize,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("output_capacity", &self.output_capacity)
            .field("balls_capacity", &self.balls_capacity)
            .finish_non_exhaustive()
    }
}

impl GpuContext {
    fn new() -> Result<Self, GpuInitError> {
        let instance = wgpu::Instance::default();
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(GpuInitError::AdapterNotFound)?;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            label: Some("fx-gpu-device"),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(GpuInitError::RequestDevice)?;

        let shader: wgpu::ShaderModule =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fx-gpu-metaballs"),
                source: wgpu::ShaderSource::Wgsl(include_str!("gpu_metaballs.wgsl").into()),
            });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx-gpu-metaballs-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx-gpu-metaballs-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fx-gpu-metaballs-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-gpu-metaballs-uniform"),
            size: std::mem::size_of::<MetaballsUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let balls_capacity = 1usize;
        let balls_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-gpu-metaballs-balls"),
            size: (balls_capacity * std::mem::size_of::<GpuBall>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output_capacity = 1usize;
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-gpu-metaballs-output"),
            size: (output_capacity * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-gpu-metaballs-readback"),
            size: (output_capacity * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-gpu-metaballs-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: balls_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            uniform_buffer,
            balls_buffer,
            output_buffer,
            readback_buffer,
            bind_group,
            output_capacity,
            balls_capacity,
        })
    }

    fn ensure_buffers(&mut self, pixel_count: usize, ball_count: usize) {
        let pixel_count = pixel_count.max(1);
        let ball_count = ball_count.max(1);

        if pixel_count > self.output_capacity {
            self.output_capacity = pixel_count;
            self.output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-gpu-metaballs-output"),
                size: (self.output_capacity * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            self.readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-gpu-metaballs-readback"),
                size: (self.output_capacity * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if ball_count > self.balls_capacity {
            self.balls_capacity = ball_count;
            self.balls_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-gpu-metaballs-balls"),
                size: (self.balls_capacity * std::mem::size_of::<GpuBall>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-gpu-metaballs-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.balls_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.output_buffer.as_entire_binding(),
                },
            ],
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_metaballs(
        &mut self,
        ctx: FxContext<'_>,
        glow: f64,
        threshold: f64,
        bg_base: PackedRgba,
        stops: [PackedRgba; 4],
        balls: &[GpuBall],
        out: &mut [PackedRgba],
    ) -> Result<(), wgpu::BufferAsyncError> {
        let pixel_count = ctx.len();
        if pixel_count == 0 {
            return Ok(());
        }
        self.ensure_buffers(pixel_count, balls.len());

        let uniform = MetaballsUniform {
            width: ctx.width as u32,
            height: ctx.height as u32,
            ball_count: balls.len() as u32,
            _pad0: 0,
            glow: glow as f32,
            threshold: threshold as f32,
            _pad1: [0.0; 2],
            bg_base: packed_to_vec4(bg_base),
            stop0: packed_to_vec4(stops[0]),
            stop1: packed_to_vec4(stops[1]),
            stop2: packed_to_vec4(stops[2]),
            stop3: packed_to_vec4(stops[3]),
        };

        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        if !balls.is_empty() {
            self.queue
                .write_buffer(&self.balls_buffer, 0, bytemuck::cast_slice(balls));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fx-gpu-metaballs-encoder"),
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fx-gpu-metaballs-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            let dispatch_x = div_ceil(ctx.width as u32, 8);
            let dispatch_y = div_ceil(ctx.height as u32, 8);
            pass.dispatch_workgroups(dispatch_x, dispatch_y, 1);
        }

        encoder.copy_buffer_to_buffer(
            &self.output_buffer,
            0,
            &self.readback_buffer,
            0,
            (pixel_count * std::mem::size_of::<u32>()) as u64,
        );

        self.queue.submit(Some(encoder.finish()));

        let slice = self
            .readback_buffer
            .slice(0..(pixel_count * std::mem::size_of::<u32>()) as u64);

        // Use channel-based callback pattern for map_async
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });

        // Poll until map completes, but avoid indefinite hangs.
        if self
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(READBACK_TIMEOUT),
            })
            .is_err()
        {
            return Err(wgpu::BufferAsyncError);
        }
        match receiver.recv_timeout(READBACK_TIMEOUT) {
            Ok(result) => result?,
            Err(_) => return Err(wgpu::BufferAsyncError),
        }

        let data = slice
            .get_mapped_range()
            .map_err(|_| wgpu::BufferAsyncError)?;
        let pixels: &[u32] = bytemuck::cast_slice(&data);
        for (dst, src) in out.iter_mut().zip(pixels.iter()) {
            *dst = PackedRgba(*src);
        }
        drop(data);
        self.readback_buffer.unmap();
        Ok(())
    }
}

#[inline]
fn packed_to_vec4(color: PackedRgba) -> [f32; 4] {
    [
        color.r() as f32 / 255.0,
        color.g() as f32 / 255.0,
        color.b() as f32 / 255.0,
        color.a() as f32 / 255.0,
    ]
}

#[inline]
fn div_ceil(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        super::gpu_test_lock()
    }

    // --- packed_to_vec4 tests ---

    #[test]
    fn packed_to_vec4_black() {
        let v = packed_to_vec4(PackedRgba::rgb(0, 0, 0));
        assert!((v[0] - 0.0).abs() < 0.001);
        assert!((v[1] - 0.0).abs() < 0.001);
        assert!((v[2] - 0.0).abs() < 0.001);
    }

    #[test]
    fn packed_to_vec4_white() {
        let v = packed_to_vec4(PackedRgba::rgb(255, 255, 255));
        assert!((v[0] - 1.0).abs() < 0.001);
        assert!((v[1] - 1.0).abs() < 0.001);
        assert!((v[2] - 1.0).abs() < 0.001);
    }

    #[test]
    fn packed_to_vec4_red() {
        let v = packed_to_vec4(PackedRgba::rgb(255, 0, 0));
        assert!((v[0] - 1.0).abs() < 0.001);
        assert!((v[1] - 0.0).abs() < 0.001);
        assert!((v[2] - 0.0).abs() < 0.001);
    }

    #[test]
    fn packed_to_vec4_mid_gray() {
        let v = packed_to_vec4(PackedRgba::rgb(128, 128, 128));
        assert!((v[0] - 128.0 / 255.0).abs() < 0.001);
        assert!((v[1] - 128.0 / 255.0).abs() < 0.001);
        assert!((v[2] - 128.0 / 255.0).abs() < 0.001);
    }

    #[test]
    fn packed_to_vec4_with_alpha() {
        let v = packed_to_vec4(PackedRgba::rgba(100, 150, 200, 128));
        assert!((v[0] - 100.0 / 255.0).abs() < 0.001);
        assert!((v[1] - 150.0 / 255.0).abs() < 0.001);
        assert!((v[2] - 200.0 / 255.0).abs() < 0.001);
        assert!((v[3] - 128.0 / 255.0).abs() < 0.001);
    }

    // --- div_ceil tests ---

    #[test]
    fn div_ceil_exact() {
        assert_eq!(div_ceil(16, 8), 2);
        assert_eq!(div_ceil(64, 8), 8);
        assert_eq!(div_ceil(0, 8), 0);
    }

    #[test]
    fn div_ceil_rounds_up() {
        assert_eq!(div_ceil(17, 8), 3);
        assert_eq!(div_ceil(1, 8), 1);
        assert_eq!(div_ceil(15, 8), 2);
    }

    #[test]
    fn div_ceil_by_one() {
        assert_eq!(div_ceil(42, 1), 42);
        assert_eq!(div_ceil(0, 1), 0);
    }

    // --- GpuBall struct tests ---

    #[test]
    fn gpu_ball_default() {
        let ball = GpuBall::default();
        assert_eq!(ball.x, 0.0);
        assert_eq!(ball.y, 0.0);
        assert_eq!(ball.r2, 0.0);
        assert_eq!(ball.hue, 0.0);
    }

    #[test]
    fn gpu_ball_clone_copy() {
        let ball = GpuBall {
            x: 1.0,
            y: 2.0,
            r2: 3.0,
            hue: 4.0,
        };
        let copied = ball;
        let copied2 = ball;
        assert_eq!(copied.x, 1.0);
        assert_eq!(copied2.y, 2.0);
    }

    #[test]
    fn gpu_ball_debug() {
        let ball = GpuBall::default();
        let debug = format!("{ball:?}");
        assert!(debug.contains("GpuBall"));
    }

    #[test]
    fn gpu_ball_pod_zeroable() {
        // Pod and Zeroable should allow safe zero-init
        let zeroed: GpuBall = bytemuck::Zeroable::zeroed();
        assert_eq!(zeroed.x, 0.0);
        assert_eq!(zeroed.y, 0.0);
    }

    // --- GpuDisableReason tests ---

    #[test]
    fn gpu_disable_reason_eq() {
        assert_eq!(GpuDisableReason::ForcedByEnv, GpuDisableReason::ForcedByEnv);
        assert_eq!(GpuDisableReason::InitFailed, GpuDisableReason::InitFailed);
        assert_eq!(
            GpuDisableReason::RenderFailed,
            GpuDisableReason::RenderFailed
        );
        assert_ne!(GpuDisableReason::ForcedByEnv, GpuDisableReason::InitFailed);
    }

    #[test]
    fn gpu_disable_reason_debug() {
        let reason = GpuDisableReason::ForcedByEnv;
        let debug = format!("{reason:?}");
        assert!(debug.contains("ForcedByEnv"));
    }

    #[test]
    fn gpu_disable_reason_clone_copy() {
        let reason = GpuDisableReason::InitFailed;
        let copied = reason;
        let copied2 = reason;
        assert_eq!(copied, GpuDisableReason::InitFailed);
        assert_eq!(copied2, GpuDisableReason::InitFailed);
    }

    // --- GpuBackend state machine tests ---

    #[test]
    fn backend_new_is_uninitialized() {
        let backend = GpuBackend::new();
        assert!(matches!(backend.state, GpuState::Uninitialized));
        assert!(!backend.is_disabled());
    }

    #[test]
    fn backend_disable_sets_unavailable() {
        let mut backend = GpuBackend::new();
        backend.disable(GpuDisableReason::ForcedByEnv);
        assert!(backend.is_disabled());
    }

    #[test]
    fn backend_disable_different_reasons() {
        let mut backend = GpuBackend::new();
        backend.disable(GpuDisableReason::InitFailed);
        assert!(backend.is_disabled());
        assert!(matches!(
            backend.state,
            GpuState::Unavailable(GpuDisableReason::InitFailed)
        ));
    }

    #[test]
    fn backend_ensure_initialized_when_already_disabled() {
        let mut backend = GpuBackend::new();
        backend.disable(GpuDisableReason::ForcedByEnv);
        let result = backend.ensure_initialized();
        assert!(result.is_err());
    }

    // --- env_truthy tests ---

    #[test]
    fn env_truthy_missing_key() {
        // Use a key unlikely to exist
        assert!(!env_truthy("FTUI_TEST_NONEXISTENT_VAR_12345"));
    }

    // --- Test helpers ---

    #[test]
    fn force_disable_and_check() {
        let _lock = test_lock();
        // Use the test helpers that manipulate global state
        force_disable_for_tests();
        assert!(is_disabled_for_tests());
        reset_for_tests();
    }

    #[test]
    fn force_init_fail_disables() {
        let _lock = test_lock();
        force_init_fail_for_tests();
        assert!(is_disabled_for_tests());
        reset_for_tests();
    }

    #[test]
    fn reset_clears_disabled_state() {
        let _lock = test_lock();
        force_disable_for_tests();
        assert!(is_disabled_for_tests());
        reset_for_tests();
        // After reset, should be uninitialized (not disabled)
        assert!(!is_disabled_for_tests());
    }

    // --- render_metaballs public function ---

    #[test]
    fn disabled_backend_returns_err_on_render() {
        use crate::visual_fx::{FxContext, FxQuality, ThemeInputs};

        // Use a local GpuBackend instead of the global to avoid race conditions
        let mut backend = GpuBackend::new();
        backend.disable(GpuDisableReason::ForcedByEnv);

        let theme = ThemeInputs::default_dark();
        let ctx = FxContext {
            width: 10,
            height: 5,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let balls = [GpuBall::default()];
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

        let result = backend.render_metaballs(
            ctx,
            1.0,
            0.5,
            PackedRgba::BLACK,
            [PackedRgba::BLACK; 4],
            &balls,
            &mut out,
        );
        assert!(
            result.is_err(),
            "disabled backend should return Err on render"
        );
    }

    // --- MetaballsUniform struct layout ---

    #[test]
    fn metaballs_uniform_pod_layout() {
        // Verify MetaballsUniform size is correct for bytemuck
        let size = std::mem::size_of::<MetaballsUniform>();
        // 4 u32s + 2 f32s + 2 pad f32s + 5 * [f32;4] = (4*4) + (2*4) + (2*4) + (5*16) = 16+8+8+80 = 112
        assert_eq!(
            size, 112,
            "MetaballsUniform should be 112 bytes for GPU alignment"
        );
    }

    #[test]
    fn gpu_ball_size() {
        // GpuBall: 4 f32s = 16 bytes
        assert_eq!(std::mem::size_of::<GpuBall>(), 16);
    }

    // --- gpu_enabled ---

    #[test]
    fn gpu_enabled_when_env_not_set() {
        // Only tests that the function is callable and returns a bool;
        // cannot mutate env vars due to #![forbid(unsafe_code)]
        let _ = gpu_enabled();
    }
}
