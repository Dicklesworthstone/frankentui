//! Profile sweep binary for flamegraph / heaptrack analysis (bd-3jlw5.7, bd-3jlw5.8, bd-h0un4).
//!
//! Renders every demo screen at 80x24 and 120x40 in a tight loop.
//! Designed to be run under `cargo flamegraph` or `heaptrack`:
//!
//!   cargo flamegraph --bin profile_sweep -p ftui-demo-showcase -- --cycles 100 --render-mode pipeline
//!   heaptrack cargo run --release --bin profile_sweep -p ftui-demo-showcase -- --cycles 10 --render-mode pipeline
//!
//! Arena comparison mode (bd-2alzw.3):
//!
//!   cargo run --release --bin profile_sweep -p ftui-demo-showcase -- --cycles 10 --render-mode pipeline --arena-mode off --json
//!   cargo run --release --bin profile_sweep -p ftui-demo-showcase -- --cycles 10 --render-mode pipeline --arena-mode on  --json

use std::alloc::System;
use std::time::Instant;

use ftui_core::event::Event;
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_demo_showcase::screens;
use ftui_render::arena::FrameArena;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_runtime::{Cmd, Model};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const PROFILE_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArenaMode {
    Off,
    On,
}

impl ArenaMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderMode {
    View,
    Pipeline,
}

impl RenderMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Pipeline => "pipeline",
        }
    }
}

#[derive(Clone, Debug)]
struct Args {
    cycles: usize,
    arena_mode: ArenaMode,
    render_mode: RenderMode,
    json: bool,
}

fn print_usage_to(mut writer: impl std::io::Write) {
    writeln!(
        writer,
        "Usage: profile_sweep [--cycles N] [--render-mode view|pipeline] [--arena-mode off|on] [--json]\n\
         Example: profile_sweep --cycles 10 --render-mode pipeline --arena-mode on --json"
    )
    .expect("writing usage should succeed");
}

fn usage_error_and_exit(message: &str) -> ! {
    eprintln!("{message}");
    print_usage_to(std::io::stderr());
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut cycles: usize = 50;
    let mut arena_mode = ArenaMode::Off;
    let mut render_mode = RenderMode::Pipeline;
    let mut json = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cycles" => {
                let Some(value) = it.next() else {
                    usage_error_and_exit("Missing value after --cycles");
                };
                cycles = value
                    .parse()
                    .unwrap_or_else(|_| usage_error_and_exit("Invalid value for --cycles"));
            }
            "--arena-mode" => {
                let Some(value) = it.next() else {
                    usage_error_and_exit("Missing value after --arena-mode");
                };
                arena_mode = match value.as_str() {
                    "off" => ArenaMode::Off,
                    "on" => ArenaMode::On,
                    _ => usage_error_and_exit("Invalid value for --arena-mode (expected off|on)"),
                };
            }
            "--render-mode" => {
                let Some(value) = it.next() else {
                    usage_error_and_exit("Missing value after --render-mode");
                };
                render_mode = match value.as_str() {
                    "view" => RenderMode::View,
                    "pipeline" => RenderMode::Pipeline,
                    _ => usage_error_and_exit(
                        "Invalid value for --render-mode (expected view|pipeline)",
                    ),
                };
            }
            "--json" => {
                json = true;
            }
            "--help" | "-h" => {
                print_usage_to(std::io::stdout());
                std::process::exit(0);
            }
            other => {
                usage_error_and_exit(&format!("Unknown argument: {other}"));
            }
        }
    }

    Args {
        cycles,
        arena_mode,
        render_mode,
        json,
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx]
}

fn sorted_copy(values: &[u64]) -> Vec<u64> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted
}

fn metric_summary_json(sorted: &[u64]) -> serde_json::Value {
    serde_json::json!({
        "p50": percentile(sorted, 0.50),
        "p95": percentile(sorted, 0.95),
        "p99": percentile(sorted, 0.99),
        "max": sorted.last().copied().unwrap_or(0),
    })
}

struct PipelineHarness {
    current: Buffer,
    scratch: Buffer,
    diff: BufferDiff,
    sink: Vec<u8>,
    caps: TerminalCapabilities,
}

impl PipelineHarness {
    fn new(cols: u16, rows: u16) -> Self {
        let mut current = Buffer::new(cols, rows);
        current.clear_dirty();
        Self {
            current,
            scratch: Buffer::new(cols, rows),
            diff: BufferDiff::new(),
            sink: Vec::with_capacity((cols as usize * rows as usize).max(4096) * 8),
            caps: TerminalCapabilities::builder()
                .color_depth(PROFILE_COLOR_DEPTH)
                .build(),
        }
    }

    fn render(
        &mut self,
        app: &mut AppModel,
        cols: u16,
        rows: u16,
        pool: &mut GraphemePool,
        arena: Option<&FrameArena>,
    ) -> (u64, usize, u64) {
        app.terminal_width = cols;
        app.terminal_height = rows;
        self.scratch.reset_for_frame();

        let mut frame = Frame::from_buffer(std::mem::take(&mut self.scratch), pool);
        if let Some(arena_ref) = arena {
            frame.set_arena(arena_ref);
        }
        app.view(&mut frame);
        self.scratch = frame.buffer;

        self.diff.compute_dirty_into(&self.current, &self.scratch);

        self.sink.clear();
        let present = {
            let mut presenter = Presenter::new(&mut self.sink, self.caps);
            presenter
                .present(&self.scratch, &self.diff)
                .expect("profile_sweep present should succeed")
        };

        let bytes_emitted = present.bytes_emitted;
        let changed_cells = present.cells_changed;
        let present_us = present.duration.as_micros().min(u64::MAX as u128) as u64;

        std::mem::swap(&mut self.current, &mut self.scratch);

        (bytes_emitted, changed_cells, present_us)
    }
}

fn pipeline_metrics_json(
    render_mode: RenderMode,
    sorted_changed_cells: &[u64],
    sorted_present_us: &[u64],
    sorted_bytes: &[u64],
) -> serde_json::Value {
    if render_mode != RenderMode::Pipeline {
        return serde_json::Value::Null;
    }

    serde_json::json!({
        "changed_cells_per_frame": {
            "p50": percentile(sorted_changed_cells, 0.50),
            "p95": percentile(sorted_changed_cells, 0.95),
            "p99": percentile(sorted_changed_cells, 0.99),
            "max": sorted_changed_cells.last().copied().unwrap_or(0)
        },
        "present_us": {
            "p50": percentile(sorted_present_us, 0.50),
            "p95": percentile(sorted_present_us, 0.95),
            "p99": percentile(sorted_present_us, 0.99),
            "max": sorted_present_us.last().copied().unwrap_or(0)
        },
        "bytes_emitted": {
            "p50": percentile(sorted_bytes, 0.50),
            "p95": percentile(sorted_bytes, 0.95),
            "p99": percentile(sorted_bytes, 0.99),
            "max": sorted_bytes.last().copied().unwrap_or(0)
        }
    })
}

struct ScreenMetrics {
    screen: ScreenId,
    frame_us: Vec<u64>,
    allocs: Vec<u64>,
    alloc_bytes: Vec<u64>,
    changed_cells: Vec<u64>,
    present_us: Vec<u64>,
    bytes: Vec<u64>,
}

impl ScreenMetrics {
    fn new(screen: ScreenId, capacity: usize) -> Self {
        Self {
            screen,
            frame_us: Vec::with_capacity(capacity),
            allocs: Vec::with_capacity(capacity),
            alloc_bytes: Vec::with_capacity(capacity),
            changed_cells: Vec::with_capacity(capacity),
            present_us: Vec::with_capacity(capacity),
            bytes: Vec::with_capacity(capacity),
        }
    }

    fn record_common(&mut self, frame_us: u64, allocs: u64, alloc_bytes: u64) {
        self.frame_us.push(frame_us);
        self.allocs.push(allocs);
        self.alloc_bytes.push(alloc_bytes);
    }

    fn record_pipeline(&mut self, bytes: u64, changed_cells: u64, present_us: u64) {
        self.bytes.push(bytes);
        self.changed_cells.push(changed_cells);
        self.present_us.push(present_us);
    }

    fn to_json(&self, render_mode: RenderMode) -> serde_json::Value {
        let sorted_frame_us = sorted_copy(&self.frame_us);
        let sorted_allocs = sorted_copy(&self.allocs);
        let sorted_alloc_bytes = sorted_copy(&self.alloc_bytes);
        let pipeline = if render_mode == RenderMode::Pipeline {
            let sorted_changed_cells = sorted_copy(&self.changed_cells);
            let sorted_present_us = sorted_copy(&self.present_us);
            let sorted_bytes = sorted_copy(&self.bytes);
            serde_json::json!({
                "changed_cells_per_frame": metric_summary_json(&sorted_changed_cells),
                "present_us": metric_summary_json(&sorted_present_us),
                "bytes_emitted": metric_summary_json(&sorted_bytes),
            })
        } else {
            serde_json::Value::Null
        };

        serde_json::json!({
            "screen": self.screen.slug(),
            "title": self.screen.title(),
            "index": self.screen.index(),
            "frames": self.frame_us.len(),
            "frame_time_us": metric_summary_json(&sorted_frame_us),
            "allocations_per_frame": metric_summary_json(&sorted_allocs),
            "allocated_bytes_per_frame": metric_summary_json(&sorted_alloc_bytes),
            "pipeline": pipeline,
        })
    }
}

fn main() {
    let args = parse_args();

    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40)];
    let screen_ids = screens::screen_ids();
    let total_frames = screen_ids.len() * sizes.len() * args.cycles;
    let per_screen_capacity = args.cycles * sizes.len();
    let mut screen_metrics = screen_ids
        .iter()
        .copied()
        .map(|screen| ScreenMetrics::new(screen, per_screen_capacity))
        .collect::<Vec<_>>();

    if !args.json {
        eprintln!(
            "Profile sweep: {} screens x {} sizes x {} cycles = {} renders (render_mode={}, arena_mode={}, color_depth={})",
            screen_ids.len(),
            sizes.len(),
            args.cycles,
            total_frames,
            args.render_mode.as_str(),
            args.arena_mode.as_str(),
            PROFILE_COLOR_DEPTH.as_str()
        );
    }

    let start = Instant::now();
    let mut pool = GraphemePool::new();
    let mut per_frame_us = Vec::with_capacity(total_frames);
    let mut per_frame_allocs = Vec::with_capacity(total_frames);
    let mut per_frame_alloc_bytes = Vec::with_capacity(total_frames);
    let mut total_allocs = 0usize;
    let mut total_alloc_bytes = 0usize;
    let mut total_reallocs = 0usize;
    let mut total_deallocs = 0usize;
    let mut per_frame_changed_cells = Vec::with_capacity(total_frames);
    let mut per_frame_present_us = Vec::with_capacity(total_frames);
    let mut per_frame_bytes = Vec::with_capacity(total_frames);
    let mut arena = (args.arena_mode == ArenaMode::On).then(|| FrameArena::new(256 * 1024));
    let mut arena_peak_bytes = 0usize;

    for &(cols, rows) in sizes {
        let mut app = AppModel::new();
        let _: Cmd<_> = app.init();
        let _: Cmd<_> = app.update(Event::Tick.into());
        let mut pipeline = PipelineHarness::new(cols, rows);

        for cycle in 0..args.cycles {
            for (screen_idx, &screen) in screen_ids.iter().enumerate() {
                app.current_screen = screen;
                let _: Cmd<_> = app.update(Event::Tick.into());
                let frame_start = Instant::now();
                let alloc_region = Region::new(GLOBAL);
                let mut bytes_emitted = 0;
                let mut changed_cells = 0;
                let mut present_us = 0;

                {
                    match args.render_mode {
                        RenderMode::View => {
                            app.terminal_width = cols;
                            app.terminal_height = rows;
                            let mut frame = Frame::new(cols, rows, &mut pool);
                            if let Some(arena_ref) = arena.as_ref() {
                                frame.set_arena(arena_ref);
                            }
                            app.view(&mut frame);
                            // Ensure the optimizer doesn't elide the render.
                            std::hint::black_box(&frame);
                        }
                        RenderMode::Pipeline => {
                            (bytes_emitted, changed_cells, present_us) =
                                pipeline.render(&mut app, cols, rows, &mut pool, arena.as_ref());
                            std::hint::black_box(bytes_emitted);
                        }
                    }
                }

                let elapsed_us = frame_start.elapsed().as_micros().min(u64::MAX as u128) as u64;
                let alloc_delta = alloc_region.change();
                let frame_allocs = alloc_delta.allocations as u64;
                let frame_alloc_bytes = alloc_delta.bytes_allocated as u64;
                per_frame_allocs.push(frame_allocs);
                per_frame_alloc_bytes.push(frame_alloc_bytes);
                total_allocs = total_allocs.saturating_add(alloc_delta.allocations);
                total_alloc_bytes = total_alloc_bytes.saturating_add(alloc_delta.bytes_allocated);
                total_reallocs = total_reallocs.saturating_add(alloc_delta.reallocations);
                total_deallocs = total_deallocs.saturating_add(alloc_delta.deallocations);

                if let Some(arena_mut) = arena.as_mut() {
                    let used = arena_mut.allocated_bytes_including_metadata();
                    arena_peak_bytes = arena_peak_bytes.max(used);
                    arena_mut.reset();
                }

                per_frame_us.push(elapsed_us);
                screen_metrics[screen_idx].record_common(
                    elapsed_us,
                    frame_allocs,
                    frame_alloc_bytes,
                );
                if args.render_mode == RenderMode::Pipeline {
                    let changed_cells = changed_cells as u64;
                    per_frame_bytes.push(bytes_emitted);
                    per_frame_changed_cells.push(changed_cells);
                    per_frame_present_us.push(present_us);
                    screen_metrics[screen_idx].record_pipeline(
                        bytes_emitted,
                        changed_cells,
                        present_us,
                    );
                }
            }
            if !args.json && cycle % 10 == 0 {
                eprint!(".");
            }
        }
    }

    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let renders_per_sec = if elapsed_secs > 0.0 {
        total_frames as f64 / elapsed_secs
    } else {
        0.0
    };

    let mut sorted_us = per_frame_us.clone();
    sorted_us.sort_unstable();
    let mut sorted_allocs = per_frame_allocs.clone();
    sorted_allocs.sort_unstable();
    let mut sorted_alloc_bytes = per_frame_alloc_bytes.clone();
    sorted_alloc_bytes.sort_unstable();

    let p50_us = percentile(&sorted_us, 0.50);
    let p95_us = percentile(&sorted_us, 0.95);
    let p99_us = percentile(&sorted_us, 0.99);
    let max_us = sorted_us.last().copied().unwrap_or(0);
    let alloc_p50 = percentile(&sorted_allocs, 0.50);
    let alloc_p95 = percentile(&sorted_allocs, 0.95);
    let alloc_p99 = percentile(&sorted_allocs, 0.99);
    let alloc_max = sorted_allocs.last().copied().unwrap_or(0);
    let alloc_bytes_p50 = percentile(&sorted_alloc_bytes, 0.50);
    let alloc_bytes_p95 = percentile(&sorted_alloc_bytes, 0.95);
    let alloc_bytes_p99 = percentile(&sorted_alloc_bytes, 0.99);
    let alloc_bytes_max = sorted_alloc_bytes.last().copied().unwrap_or(0);
    let mut sorted_present_us = per_frame_present_us.clone();
    sorted_present_us.sort_unstable();
    let mut sorted_changed_cells = per_frame_changed_cells.clone();
    sorted_changed_cells.sort_unstable();
    let mut sorted_bytes = per_frame_bytes.clone();
    sorted_bytes.sort_unstable();

    if args.json {
        let summary = serde_json::json!({
            "arena_mode": args.arena_mode.as_str(),
            "render_mode": args.render_mode.as_str(),
            "color_depth": PROFILE_COLOR_DEPTH.as_str(),
            "cycles": args.cycles,
            "screen_count": screen_ids.len(),
            "sizes": sizes.iter().map(|(w, h)| serde_json::json!({"cols": w, "rows": h})).collect::<Vec<_>>(),
            "total_frames": total_frames,
            "elapsed_ms": elapsed_secs * 1000.0,
            "renders_per_sec": renders_per_sec,
            "frame_time_us": {
                "p50": p50_us,
                "p95": p95_us,
                "p99": p99_us,
                "max": max_us
            },
            "allocations": {
                "total": total_allocs,
                "reallocations_total": total_reallocs,
                "deallocations_total": total_deallocs,
                "per_frame": {
                    "p50": alloc_p50,
                    "p95": alloc_p95,
                    "p99": alloc_p99,
                    "max": alloc_max
                }
            },
            "allocated_bytes": {
                "total": total_alloc_bytes,
                "per_frame": {
                    "p50": alloc_bytes_p50,
                    "p95": alloc_bytes_p95,
                    "p99": alloc_bytes_p99,
                    "max": alloc_bytes_max
                }
            },
            "arena_peak_bytes": arena_peak_bytes,
            "pipeline": pipeline_metrics_json(
                args.render_mode,
                &sorted_changed_cells,
                &sorted_present_us,
                &sorted_bytes
            ),
            "screens": screen_metrics
                .iter()
                .map(|metrics| metrics.to_json(args.render_mode))
                .collect::<Vec<_>>()
        });
        println!("{summary}");
    } else {
        let mut summary = format!(
            "\nDone in {:.2}s ({:.1} renders/sec) | mode={} | color_depth={} | frame_us p50={} p95={} p99={} max={} | allocs/frame p50={} p95={} p99={} max={} | arena_peak_bytes={}",
            elapsed_secs,
            renders_per_sec,
            args.render_mode.as_str(),
            PROFILE_COLOR_DEPTH.as_str(),
            p50_us,
            p95_us,
            p99_us,
            max_us,
            alloc_p50,
            alloc_p95,
            alloc_p99,
            alloc_max,
            arena_peak_bytes
        );
        if args.render_mode == RenderMode::Pipeline {
            summary.push_str(&format!(
                " | bytes/frame p50={} p95={} p99={} max={} | changed_cells/frame p50={} p95={} p99={} max={} | present_us p50={} p95={} p99={} max={}",
                percentile(&sorted_bytes, 0.50),
                percentile(&sorted_bytes, 0.95),
                percentile(&sorted_bytes, 0.99),
                sorted_bytes.last().copied().unwrap_or(0),
                percentile(&sorted_changed_cells, 0.50),
                percentile(&sorted_changed_cells, 0.95),
                percentile(&sorted_changed_cells, 0.99),
                sorted_changed_cells.last().copied().unwrap_or(0),
                percentile(&sorted_present_us, 0.50),
                percentile(&sorted_present_us, 0.95),
                percentile(&sorted_present_us, 0.99),
                sorted_present_us.last().copied().unwrap_or(0),
            ));
        }
        eprintln!("{summary}");
    }
}
