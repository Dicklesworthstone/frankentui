//! Benchmarks for the production-faithful demo render pipeline (bd-h0un4).
//!
//! Each measured iteration runs exactly 200 `Tick -> view -> diff -> present`
//! updates for a fresh screen, reusing buffers, diff storage, the grapheme pool,
//! and the ANSI sink throughout. Initialization and teardown are excluded so
//! adaptive sampling cannot change the trajectory measured by each iteration.

#![forbid(unsafe_code)]

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;
use ftui_render::presenter::Presenter;
use ftui_runtime::{Cmd, Model};
use std::hint::black_box;
use std::time::{Duration, Instant};

const BENCHMARK_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;
const TICKS_PER_ITERATION: u64 = 200;

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
                .color_depth(BENCHMARK_COLOR_DEPTH)
                .osc8_hyperlinks(true)
                .build(),
            links: LinkRegistry::new(),
        }
    }

    fn render(&mut self, app: &mut AppModel, cols: u16, rows: u16, pool: &mut GraphemePool) {
        app.terminal_width = cols;
        app.terminal_height = rows;
        self.scratch.reset_for_frame();

        let mut frame = Frame::from_buffer(std::mem::take(&mut self.scratch), pool);
        frame.set_links(&mut self.links);
        app.view(&mut frame);
        self.scratch = frame.buffer;

        self.diff.compute_dirty_into(&self.current, &self.scratch);

        self.sink.clear();
        {
            let mut presenter = Presenter::new(&mut self.sink, self.caps);
            presenter
                .present_with_pool(&self.scratch, &self.diff, Some(pool), Some(&self.links))
                .expect("demo pipeline bench present should succeed");
        }

        std::mem::swap(&mut self.current, &mut self.scratch);
    }
}

fn benchmark_screens() -> &'static [(ScreenId, &'static str)] {
    &[
        (ScreenId::Dashboard, "dashboard"),
        (ScreenId::WidgetGallery, "widget_gallery"),
        (ScreenId::LayoutLab, "layout_lab"),
        (ScreenId::DataViz, "data_viz"),
        (ScreenId::Performance, "performance"),
    ]
}

fn bench_demo_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("demo_pipeline/truecolor/reused_buffer_200_ticks");
    group.sampling_mode(SamplingMode::Flat);

    for &(cols, rows) in &[(80, 24), (120, 40)] {
        let cells = cols as u64 * rows as u64;
        for &(screen, label) in benchmark_screens() {
            group.throughput(Throughput::Elements(cells * TICKS_PER_ITERATION));
            group.bench_with_input(
                BenchmarkId::new(label, format!("{cols}x{rows}")),
                &(screen, cols, rows),
                |b, &(screen, cols, rows)| {
                    b.iter_custom(|iterations| {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            // Initialize the same deterministic starting state for
                            // every trajectory, outside its measured interval.
                            let mut app = AppModel::new();
                            let _: Cmd<_> = app.init();
                            app.current_screen = screen;
                            let _: Cmd<_> = app.update(Event::Tick.into());
                            let mut pool = GraphemePool::new();
                            let mut pipeline = PipelineHarness::new(cols, rows);

                            let started = Instant::now();
                            for _ in 0..TICKS_PER_ITERATION {
                                let _: Cmd<_> = app.update(Event::Tick.into());
                                pipeline.render(&mut app, cols, rows, &mut pool);
                                black_box(pipeline.diff.len());
                                black_box(pipeline.sink.len());
                            }
                            elapsed += started.elapsed();
                            // Model and retained render state drop after timing.
                        }
                        elapsed
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_demo_pipeline);
criterion_main!(benches);
