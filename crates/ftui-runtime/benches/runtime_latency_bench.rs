#![forbid(unsafe_code)]

//! User-visible runtime lifecycle latency benchmarks (bd-g00-root-epic-ewths.31.2).
//!
//! Run with:
//! `cargo bench -p ftui-runtime --bench runtime_latency_bench`
//!
//! Covers the three lifecycle rows in `tests/baseline.json`:
//! - `runtime/first_frame/headless_80x24`
//! - `runtime/shutdown_latency/headless`
//! - `runtime/command_roundtrip/cmd_msg`

use criterion::{Criterion, criterion_group};
use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::simulator::ProgramSimulator;
use ftui_runtime::{Cmd, Model, Program, ProgramConfig};
use std::hint::black_box;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum LatencyMsg {
    TriggerRoundtrip,
    RoundtripCompleted,
    Quit,
    Noop,
}

impl From<Event> for LatencyMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('r'),
                ..
            }) => Self::TriggerRoundtrip,
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                ..
            }) => Self::Quit,
            _ => Self::Noop,
        }
    }
}

#[derive(Default)]
struct LatencyModel {
    roundtrips: u64,
}

impl Model for LatencyModel {
    type Message = LatencyMsg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            LatencyMsg::TriggerRoundtrip => Cmd::msg(LatencyMsg::RoundtripCompleted),
            LatencyMsg::RoundtripCompleted => {
                self.roundtrips = self.roundtrips.wrapping_add(1);
                Cmd::none()
            }
            LatencyMsg::Quit => Cmd::quit(),
            LatencyMsg::Noop => Cmd::none(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let label = b"FrankenTUI Runtime Latency Benchmark";
        for (i, &b) in label.iter().enumerate() {
            frame
                .buffer
                .set_raw(i as u16, 0, Cell::from_char(b as char));
        }
    }
}

fn headless_config() -> ProgramConfig {
    ProgramConfig::fullscreen()
        .with_forced_size(80, 24)
        .with_signal_interception(false)
}

fn bench_first_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/first_frame");
    group.bench_function("headless_80x24", |b| {
        b.iter(|| {
            let mut program = Program::new_headless(LatencyModel::default(), headless_config())
                .expect("construct headless program");
            program
                .init_headless(Duration::from_secs(1))
                .expect("render first headless frame");
            black_box(program.model().roundtrips)
        });
    });
    group.finish();
}

fn bench_shutdown_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/shutdown_latency");
    group.bench_function("headless", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut program = Program::new_headless(LatencyModel::default(), headless_config())
                    .expect("construct headless program");
                program
                    .init_headless(Duration::from_secs(1))
                    .expect("init headless program");

                let start = Instant::now();
                program
                    .step_headless_event(
                        Event::Key(KeyEvent::new(KeyCode::Char('q'))),
                        Duration::from_secs(1),
                    )
                    .expect("deliver quit event");
                drop(program);
                total += start.elapsed();
            }
            total
        });
    });
    group.finish();
}

fn bench_command_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/command_roundtrip");
    group.bench_function("cmd_msg", |b| {
        let mut sim = ProgramSimulator::new(LatencyModel::default());
        sim.init();
        let event = [Event::Key(KeyEvent::new(KeyCode::Char('r')))];
        b.iter(|| {
            sim.inject_events(black_box(&event));
            black_box(sim.model().roundtrips)
        });
    });
    group.finish();
}

fn emit_criterion_jsonl(bench_name: &str, ids: &[&str]) {
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
    for &id in ids {
        let estimates_path = std::path::Path::new(&target_dir)
            .join("criterion")
            .join(id)
            .join("new")
            .join("estimates.json");
        if let Ok(text) = std::fs::read_to_string(&estimates_path)
            && let Some(mean_ns) = extract_point_estimate(&text, "mean")
        {
            let p50_ns = extract_point_estimate(&text, "median").unwrap_or(mean_ns);
            let p99_ns = extract_upper_ci(&text, "mean").unwrap_or(mean_ns);
            eprintln!(
                r#"{{"bench":"{bench_name}","id":"{id}","mean_ns":{mean_ns:.2},"p50_ns":{p50_ns:.2},"p99_ns":{p99_ns:.2}}}"#
            );
        }
    }
}

fn extract_point_estimate(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let pe_key = "\"point_estimate\":";
    let pe_start = rest.find(pe_key)? + pe_key.len();
    let tail = rest[pe_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

fn extract_upper_ci(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let ci_key = "\"upper_bound\":";
    let ci_start = rest.find(ci_key)? + ci_key.len();
    let tail = rest[ci_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

criterion_group!(
    benches,
    bench_first_frame,
    bench_shutdown_latency,
    bench_command_roundtrip,
);

fn main() {
    benches();
    emit_criterion_jsonl(
        "runtime_latency_bench",
        &[
            "runtime/first_frame/headless_80x24",
            "runtime/shutdown_latency/headless",
            "runtime/command_roundtrip/cmd_msg",
        ],
    );
}
