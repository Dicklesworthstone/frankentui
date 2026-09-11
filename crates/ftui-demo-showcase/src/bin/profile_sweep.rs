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
use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
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
use ftui_render::link_registry::LinkRegistry;
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
    capture_ansi: Option<PathBuf>,
}

fn print_usage_to(mut writer: impl std::io::Write) {
    writeln!(
        writer,
        "Usage: profile_sweep [--cycles N] [--render-mode view|pipeline] [--arena-mode off|on] [--json] [--capture-ansi PATH]\n\
         --capture-ansi requires pipeline mode and creates a new retained binary capture file.\n\
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
    let mut capture_ansi = None;

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
            "--capture-ansi" => {
                if capture_ansi.is_some() {
                    usage_error_and_exit("--capture-ansi may only be specified once");
                }
                let Some(value) = it.next() else {
                    usage_error_and_exit("Missing value after --capture-ansi");
                };
                if value.is_empty() {
                    usage_error_and_exit("--capture-ansi requires a nonempty path");
                }
                capture_ansi = Some(PathBuf::from(value));
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

    if cycles == 0 {
        usage_error_and_exit("--cycles must be greater than zero");
    }
    if capture_ansi.is_some() && render_mode != RenderMode::Pipeline {
        usage_error_and_exit("--capture-ansi requires --render-mode pipeline");
    }

    Args {
        cycles,
        arena_mode,
        render_mode,
        json,
        capture_ansi,
    }
}

/// Retained ANSI stream, version 1. All integers are little-endian.
///
/// Header: `FTUIANSI`, version u16, expected frame count u64.
/// Each record: payload length u64, columns u16, rows u16, zero-based cycle u64,
/// UTF-8 screen slug length u32, ANSI length u64, slug bytes, then exact ANSI bytes.
/// The payload length excludes its own u64 prefix. Readers must require the
/// declared frame count and EOF immediately after the final record.
struct AnsiCapture<W> {
    writer: W,
    expected_frames: u64,
    written_frames: u64,
}

impl<W: Write> AnsiCapture<W> {
    fn new(mut writer: W, expected_frames: u64) -> io::Result<Self> {
        if expected_frames == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty ANSI capture",
            ));
        }
        writer.write_all(b"FTUIANSI")?;
        writer.write_all(&1u16.to_le_bytes())?;
        writer.write_all(&expected_frames.to_le_bytes())?;
        Ok(Self {
            writer,
            expected_frames,
            written_frames: 0,
        })
    }

    fn write_frame(
        &mut self,
        cols: u16,
        rows: u16,
        screen: &str,
        cycle: u64,
        ansi: &[u8],
    ) -> io::Result<()> {
        if self.written_frames >= self.expected_frames
            || cols == 0
            || rows == 0
            || screen.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid ANSI frame identity",
            ));
        }
        let screen_len = u32::try_from(screen.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "ANSI screen slug too long")
        })?;
        let ansi_len = u64::try_from(ansi.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ANSI frame too long"))?;
        let payload_len = 24u64
            .checked_add(u64::from(screen_len))
            .and_then(|length| length.checked_add(ansi_len))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ANSI record too long"))?;

        self.writer.write_all(&payload_len.to_le_bytes())?;
        self.writer.write_all(&cols.to_le_bytes())?;
        self.writer.write_all(&rows.to_le_bytes())?;
        self.writer.write_all(&cycle.to_le_bytes())?;
        self.writer.write_all(&screen_len.to_le_bytes())?;
        self.writer.write_all(&ansi_len.to_le_bytes())?;
        self.writer.write_all(screen.as_bytes())?;
        self.writer.write_all(ansi)?;
        self.written_frames += 1;
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        if self.written_frames != self.expected_frames {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete ANSI capture",
            ));
        }
        self.writer.flush()?;
        Ok(self.writer)
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
        "p90": percentile(sorted, 0.90),
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
    links: LinkRegistry,
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
                .osc8_hyperlinks(true)
                .build(),
            links: LinkRegistry::new(),
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
        frame.set_links(&mut self.links);
        if let Some(arena_ref) = arena {
            frame.set_arena(arena_ref);
        }
        app.view(&mut frame);
        self.scratch = frame.buffer;

        self.present(pool)
    }

    fn present(&mut self, pool: &GraphemePool) -> (u64, usize, u64) {
        self.diff.compute_dirty_into(&self.current, &self.scratch);

        self.sink.clear();
        let present = {
            let mut presenter = Presenter::new(&mut self.sink, self.caps);
            presenter
                .present_with_pool(&self.scratch, &self.diff, Some(pool), Some(&self.links))
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
        "changed_cells_total": sorted_changed_cells.iter().sum::<u64>(),
        "bytes_emitted_total": sorted_bytes.iter().sum::<u64>(),
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
                "changed_cells_total": self.changed_cells.iter().sum::<u64>(),
                "bytes_emitted_total": self.bytes.iter().sum::<u64>(),
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

fn main() -> io::Result<()> {
    let args = parse_args();

    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40)];
    let screen_ids = screens::screen_ids();
    let total_frames = screen_ids
        .len()
        .checked_mul(sizes.len())
        .and_then(|frames| frames.checked_mul(args.cycles))
        .unwrap_or_else(|| usage_error_and_exit("--cycles exceeds the supported frame count"));
    let mut capture = args
        .capture_ansi
        .as_ref()
        .map(|path| {
            let expected_frames = u64::try_from(total_frames).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "ANSI frame count too large")
            })?;
            let file = OpenOptions::new().write(true).create_new(true).open(path)?;
            AnsiCapture::new(BufWriter::new(file), expected_frames)
        })
        .transpose()?;
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
                // The sink already owns the full Presenter output. Capture only
                // after both measurements, with all capture work behind this branch.
                if let Some(capture) = capture.as_mut() {
                    capture.write_frame(
                        cols,
                        rows,
                        screen.slug(),
                        u64::try_from(cycle).expect("cycle fits declared frame count"),
                        &pipeline.sink,
                    )?;
                }
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

    if let Some(capture) = capture {
        capture.finish()?.get_ref().sync_all()?;
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
        let mut summary = serde_json::json!({
            "arena_mode": args.arena_mode.as_str(),
            "render_mode": args.render_mode.as_str(),
            "color_depth": PROFILE_COLOR_DEPTH.as_str(),
            "osc8_hyperlinks": true,
            "presenter_context": "grapheme_pool_and_link_registry",
            "cycles": args.cycles,
            "screen_count": screen_ids.len(),
            "sizes": sizes.iter().map(|(w, h)| serde_json::json!({"cols": w, "rows": h})).collect::<Vec<_>>(),
            "total_frames": total_frames,
            "elapsed_ms": elapsed_secs * 1000.0,
            "renders_per_sec": renders_per_sec,
            "frame_time_us": {
                "p50": p50_us,
                "p90": percentile(&sorted_us, 0.90),
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
        if let Some(path) = &args.capture_ansi {
            summary["ansi_capture"] = serde_json::json!({
                "path": path,
                "format": "FTUIANSI",
                "version": 1,
                "frames": total_frames,
                "included_in_frame_metrics": false,
                "included_in_elapsed_time": true,
            });
        }
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
        if let Some(path) = &args.capture_ansi {
            eprintln!(
                "ANSI capture: {} (FTUIANSI v1, {} frames; capture I/O is included only in overall elapsed time)",
                path.display(),
                total_frames
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::{Cell, CellContent};

    #[test]
    fn pipeline_preserves_pooled_graphemes_and_registered_links() {
        let mut harness = PipelineHarness::new(8, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::from_buffer(std::mem::take(&mut harness.scratch), &mut pool);
        frame.set_links(&mut harness.links);
        let link = frame.register_link("https://example.com/profile");
        assert_ne!(link, 0);
        let grapheme = frame.pool.intern("e\u{301}", 1);
        let mut cell = Cell::new(CellContent::from_grapheme(grapheme));
        cell.attrs = cell.attrs.with_link(link);
        frame.buffer.set(0, 0, cell);
        harness.scratch = frame.buffer;

        // This was the old harness path: the same cell loses its pooled text
        // and URL when presentation receives neither registry.
        let diff = BufferDiff::compute(&harness.current, &harness.scratch);
        let mut missing_context = Vec::new();
        Presenter::new(&mut missing_context, harness.caps)
            .present(&harness.scratch, &diff)
            .unwrap();
        assert!(
            !String::from_utf8(missing_context)
                .unwrap()
                .contains("e\u{301}")
        );

        let (bytes, changed, _) = harness.present(&pool);
        assert_eq!(bytes as usize, harness.sink.len());
        assert_eq!(changed, 1);
        let ansi = std::str::from_utf8(&harness.sink).unwrap();
        assert!(ansi.contains("e\u{301}"), "{ansi:?}");
        assert!(ansi.contains("https://example.com/profile"), "{ansi:?}");
        assert!(
            ansi.contains("\u{1b}]8;;\u{7}"),
            "link must close: {ansi:?}"
        );

        let mut capture = AnsiCapture::new(Vec::new(), 1).unwrap();
        capture
            .write_frame(8, 1, "pooled", 0, &harness.sink)
            .unwrap();
        let captured = capture.finish().unwrap();
        // Fixed header (18), record prefix (8), identity fields (24), slug (6).
        assert_eq!(&captured[56..], harness.sink.as_slice());
        assert_eq!(captured.len(), 56 + harness.sink.len());

        harness.scratch.reset_for_frame();
        harness.scratch.set(0, 0, cell);
        let (_, unchanged, _) = harness.present(&pool);
        assert_eq!(unchanged, 0);
    }

    #[test]
    fn ansi_capture_has_exact_versioned_frame_boundaries() {
        let mut capture = AnsiCapture::new(Vec::new(), 2).unwrap();
        capture.write_frame(80, 24, "a", 9, b"\x1b[31mA\n").unwrap();
        capture
            .write_frame(120, 40, "界", 10, &[0xff, 0, 0x1b])
            .unwrap();
        let bytes = capture.finish().unwrap();

        let expected = [
            b"FTUIANSI\x01\0\x02\0\0\0\0\0\0\0".as_slice(),
            // First payload: 24 fixed bytes + 1 slug byte + 7 ANSI bytes = 32.
            b"\x20\0\0\0\0\0\0\0\x50\0\x18\0\x09\0\0\0\0\0\0\0".as_slice(),
            b"\x01\0\0\0\x07\0\0\0\0\0\0\0a\x1b[31mA\n".as_slice(),
            // Second payload: UTF-8 slug length is 3 bytes, not 1 character.
            b"\x1e\0\0\0\0\0\0\0\x78\0\x28\0\x0a\0\0\0\0\0\0\0".as_slice(),
            b"\x03\0\0\0\x03\0\0\0\0\0\0\0".as_slice(),
            "界".as_bytes(),
            &[0xff, 0, 0x1b],
        ]
        .concat();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn ansi_capture_accepts_empty_output_but_rejects_incomplete_or_extra_frames() {
        assert!(AnsiCapture::new(Vec::new(), 0).is_err());
        let mut capture = AnsiCapture::new(Vec::new(), 2).unwrap();
        capture.write_frame(80, 24, "empty", 0, &[]).unwrap();
        assert_eq!(
            capture.finish().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut capture = AnsiCapture::new(Vec::new(), 1).unwrap();
        capture.write_frame(80, 24, "empty", 0, &[]).unwrap();
        let completed_len = capture.writer.len();
        assert!(capture.write_frame(80, 24, "extra", 1, b"x").is_err());
        assert_eq!(capture.writer.len(), completed_len);
        let bytes = capture.finish().unwrap();
        assert_eq!(bytes.len(), 18 + 8 + 24 + 5);
        assert_eq!(&bytes[42..50], &[0; 8]); // Empty ANSI payload length.
    }

    #[test]
    fn ansi_capture_rejects_invalid_identity_and_propagates_write_errors() {
        let mut capture = AnsiCapture::new(Vec::new(), 1).unwrap();
        for (cols, rows, screen) in [(0, 24, "screen"), (80, 0, "screen"), (80, 24, "")] {
            assert!(capture.write_frame(cols, rows, screen, 0, b"ansi").is_err());
            assert_eq!(capture.writer.len(), 18); // Header only; no partial record.
        }

        let mut header_only = [0u8; 18];
        let writer = io::Cursor::new(header_only.as_mut_slice());
        let mut capture = AnsiCapture::new(writer, 1).unwrap();
        let error = capture
            .write_frame(80, 24, "screen", 0, b"ansi")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!(capture.written_frames, 0);
    }
}
