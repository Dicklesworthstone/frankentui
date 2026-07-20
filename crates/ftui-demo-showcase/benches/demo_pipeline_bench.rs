//! Benchmarks for the production-faithful demo render pipeline (bd-h0un4).
//!
//! Measures `view -> diff -> present` for real ftui-demo-showcase screens while
//! reusing buffers, diff storage, and the ANSI sink so results reflect the live
//! render path rather than per-frame allocation noise.

#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_runtime::{Cmd, Model};
use std::hint::black_box;

const BENCHMARK_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

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
                .color_depth(BENCHMARK_COLOR_DEPTH)
                .build(),
        }
    }

    fn render(&mut self, app: &mut AppModel, cols: u16, rows: u16, pool: &mut GraphemePool) {
        app.terminal_width = cols;
        app.terminal_height = rows;
        self.scratch.reset_for_frame();

        let mut frame = Frame::from_buffer(std::mem::take(&mut self.scratch), pool);
        app.view(&mut frame);
        self.scratch = frame.buffer;

        self.diff.compute_dirty_into(&self.current, &self.scratch);

        self.sink.clear();
        {
            let mut presenter = Presenter::new(&mut self.sink, self.caps);
            presenter
                .present(&self.scratch, &self.diff)
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
    let mut group = c.benchmark_group("demo_pipeline/truecolor/reused_buffer");

    for &(cols, rows) in &[(80, 24), (120, 40)] {
        let cells = cols as u64 * rows as u64;
        for &(screen, label) in benchmark_screens() {
            group.throughput(Throughput::Elements(cells));
            group.bench_with_input(
                BenchmarkId::new(label, format!("{cols}x{rows}")),
                &(screen, cols, rows),
                |b, &(screen, cols, rows)| {
                    let mut app = AppModel::new();
                    let _: Cmd<_> = app.init();
                    app.current_screen = screen;
                    let _: Cmd<_> = app.update(Event::Tick.into());
                    let mut pool = GraphemePool::new();
                    let mut pipeline = PipelineHarness::new(cols, rows);

                    b.iter(|| {
                        let _: Cmd<_> = app.update(Event::Tick.into());
                        pipeline.render(&mut app, cols, rows, &mut pool);
                        black_box(pipeline.diff.len());
                        black_box(pipeline.sink.len());
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_demo_pipeline);
criterion_main!(benches);
