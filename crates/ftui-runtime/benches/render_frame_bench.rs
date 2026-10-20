//! Benchmark for headless `Program::render_frame()` at 80x24 with conformal
//! frame-time risk gating off vs. on (default).
//!
//! Run with:
//! `cargo bench -p ftui-runtime --bench render_frame_bench`

use criterion::{Criterion, criterion_group, criterion_main};
use ftui_backend::BackendFeatures;
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::{
    Cmd, HeadlessEventSource, Model, Program, ProgramConfig, TerminalPresenter, TerminalWriter,
};
use std::cell::Cell as StdCell;
use std::hint::black_box;
use std::io::{self, Write};

/// A zero-allocation sink writer so the benchmark measures `Program::render_frame`
/// (including conformal prediction + observation, budget controller, buffer diff,
/// and presenter emission) without unbounded `Vec<u8>` growth across iterations.
struct SinkWriter(usize);

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.wrapping_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BenchModel {
    tick: StdCell<u32>,
}

#[derive(Debug)]
struct BenchMsg;

impl From<Event> for BenchMsg {
    fn from(_: Event) -> Self {
        Self
    }
}

impl Model for BenchModel {
    type Message = BenchMsg;

    fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let t = self.tick.get().wrapping_add(1);
        self.tick.set(t);
        let ch = char::from(b'A' + (t % 26) as u8);
        // Mutate ~5% of an 80x24 grid (96 cells across 4 rows) to exercise the
        // realistic steady-state frame path.
        for row in 0..4u16 {
            for col in 0..24u16 {
                frame.buffer.set_raw(col * 3, row * 6, Cell::from_char(ch));
            }
        }
    }
}

fn build_headless_80x24(
    config: ProgramConfig,
) -> Program<BenchModel, HeadlessEventSource, TerminalPresenter<SinkWriter>> {
    let config = config
        .with_forced_size(80, 24)
        .with_signal_interception(false);
    let writer = TerminalWriter::with_diff_config(
        SinkWriter(0),
        config.screen_mode,
        config.ui_anchor,
        TerminalCapabilities::basic(),
        config.diff_config.clone(),
    );
    let presenter = TerminalPresenter::new(writer);
    let events = HeadlessEventSource::new(80, 24, BackendFeatures::default());
    let mut program = Program::with_event_source(
        BenchModel {
            tick: StdCell::new(0),
        },
        events,
        BackendFeatures::default(),
        presenter,
        config,
    )
    .expect("construct headless benchmark program");

    // Pre-warm 300 frames so the 256-sample conformal window is full and
    // steady-state calibrated quantile computation runs on every iteration.
    for _ in 0..300 {
        program.step_headless_frame().expect("warmup frame");
    }
    program
}

fn bench_render_frame_headless_80x24(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/render_frame_headless_80x24");

    group.bench_function("conformal_off", |b| {
        let mut program = build_headless_80x24(ProgramConfig::fullscreen().without_conformal());
        b.iter(|| {
            program.step_headless_frame().expect("step_headless_frame");
            black_box(())
        });
    });

    group.bench_function("conformal_on", |b| {
        let mut program = build_headless_80x24(ProgramConfig::fullscreen());
        b.iter(|| {
            program.step_headless_frame().expect("step_headless_frame");
            black_box(())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_render_frame_headless_80x24);
criterion_main!(benches);
