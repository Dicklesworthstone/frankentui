#![forbid(unsafe_code)]

//! Criterion benchmark suite comparing task executor backends (`Spawned`,
//! `EffectQueue`, and feature-gated `Asupersync`) across three workloads:
//!
//! 1. `runtime/effect_queue_drain/{spawned,effect_queue,asupersync}`:
//!    Enqueue 50 trivial tasks and measure time to drain all completions.
//! 2. `runtime/task_burst_200/{spawned,effect_queue,asupersync}`:
//!    Submit 200 tasks performing ~100 µs of simulated work in one update
//!    batch and measure drain latency (emitting p50/p99 JSONL summary).
//! 3. `runtime/pollers_10x10ms/{spawned,effect_queue,asupersync}`:
//!    Issue #78 guard workload: 10 concurrent 10 ms polling tasks running
//!    alongside a keystroke-echo task; measures echo task latency while
//!    polling tasks occupy the executor.
//!
//! Run with:
//! `cargo bench -p ftui-runtime --bench effect_backend_bench`

use criterion::{Criterion, criterion_group, criterion_main};
use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_render::frame::Frame;
use ftui_runtime::{
    Cmd, EffectQueueConfig, HeadlessEventSource, Model, Program, ProgramConfig, RuntimeLane,
    TaskExecutorBackend, TerminalPresenter,
};
use std::hint::black_box;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum BenchMsg {
    EnqueueDrainBatch(usize),
    EnqueueBurst(usize),
    StartPollersAndEcho { pollers: usize, rounds_left: usize },
    PollerTick { id: usize, rounds_left: usize },
    EchoCompleted(Duration),
    TaskDone(Duration),
    Noop,
}

impl From<Event> for BenchMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('d'),
                ..
            }) => Self::EnqueueDrainBatch(50),
            Event::Key(KeyEvent {
                code: KeyCode::Char('b'),
                ..
            }) => Self::EnqueueBurst(200),
            Event::Key(KeyEvent {
                code: KeyCode::Char('p'),
                ..
            }) => Self::StartPollersAndEcho {
                pollers: 10,
                rounds_left: 1,
            },
            _ => Self::Noop,
        }
    }
}

struct BenchModel {
    completed: usize,
    latencies_us: Vec<u64>,
    echo_latencies_us: Vec<u64>,
}

impl BenchModel {
    fn new() -> Self {
        Self {
            completed: 0,
            latencies_us: Vec::with_capacity(256),
            echo_latencies_us: Vec::with_capacity(64),
        }
    }
}

fn spin_work(duration: Duration) {
    let start = Instant::now();
    let mut acc = 0u64;
    while start.elapsed() < duration {
        acc = acc.wrapping_add(1);
        std::hint::spin_loop();
    }
    black_box(acc);
}

impl Model for BenchModel {
    type Message = BenchMsg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            BenchMsg::EnqueueDrainBatch(count) => {
                let cmds: Vec<Cmd<Self::Message>> = (0..count)
                    .map(|_| {
                        let submitted = Instant::now();
                        Cmd::task(move || BenchMsg::TaskDone(submitted.elapsed()))
                    })
                    .collect();
                Cmd::batch(cmds)
            }
            BenchMsg::EnqueueBurst(count) => {
                let cmds: Vec<Cmd<Self::Message>> = (0..count)
                    .map(|_| {
                        let submitted = Instant::now();
                        Cmd::task(move || {
                            spin_work(Duration::from_micros(100));
                            BenchMsg::TaskDone(submitted.elapsed())
                        })
                    })
                    .collect();
                Cmd::batch(cmds)
            }
            BenchMsg::StartPollersAndEcho {
                pollers,
                rounds_left,
            } => {
                let mut cmds: Vec<Cmd<Self::Message>> = (0..pollers)
                    .map(|id| {
                        Cmd::task(move || {
                            std::thread::sleep(Duration::from_millis(10));
                            BenchMsg::PollerTick {
                                id,
                                rounds_left: rounds_left.saturating_sub(1),
                            }
                        })
                    })
                    .collect();
                let echo_submitted = Instant::now();
                cmds.push(Cmd::task(move || {
                    BenchMsg::EchoCompleted(echo_submitted.elapsed())
                }));
                Cmd::batch(cmds)
            }
            BenchMsg::PollerTick { id, rounds_left } => {
                self.completed += 1;
                if rounds_left == 0 {
                    Cmd::none()
                } else {
                    Cmd::task(move || {
                        std::thread::sleep(Duration::from_millis(10));
                        BenchMsg::PollerTick {
                            id,
                            rounds_left: rounds_left - 1,
                        }
                    })
                }
            }
            BenchMsg::EchoCompleted(elapsed) => {
                self.echo_latencies_us.push(elapsed.as_micros() as u64);
                Cmd::none()
            }
            BenchMsg::TaskDone(elapsed) => {
                self.completed += 1;
                self.latencies_us.push(elapsed.as_micros() as u64);
                Cmd::none()
            }
            BenchMsg::Noop => Cmd::none(),
        }
    }

    fn view(&self, _frame: &mut Frame) {}
}

fn build_program(
    lane: RuntimeLane,
    backend: TaskExecutorBackend,
) -> Program<BenchModel, HeadlessEventSource, TerminalPresenter<Vec<u8>>> {
    let config = ProgramConfig::default()
        .with_forced_size(80, 24)
        .with_lane(lane)
        .with_effect_queue(EffectQueueConfig::default().with_backend(backend))
        .with_signal_interception(false);
    Program::new_headless(BenchModel::new(), config).expect("build headless bench program")
}

fn percentile_us(samples: &mut [u64], pct: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = ((samples.len() - 1) as f64 * pct).round() as usize;
    samples[idx.min(samples.len() - 1)]
}

fn emit_summary(bench: &str, backend: &str, samples: &mut [u64], total_ms: f64) {
    let p50 = percentile_us(samples, 0.50);
    let p99 = percentile_us(samples, 0.99);
    eprintln!(
        r#"{{"bench":"{bench}","backend":"{backend}","p50_us":{p50},"p99_us":{p99},"total_ms":{total_ms:.3}}}"#
    );
}

fn backends_to_bench() -> Vec<(&'static str, RuntimeLane, TaskExecutorBackend)> {
    #[allow(unused_mut)]
    let mut list = vec![
        (
            "spawned",
            RuntimeLane::Structured,
            TaskExecutorBackend::Spawned,
        ),
        (
            "effect_queue",
            RuntimeLane::Structured,
            TaskExecutorBackend::EffectQueue,
        ),
    ];
    #[cfg(feature = "asupersync-executor")]
    list.push((
        "asupersync",
        RuntimeLane::Asupersync,
        TaskExecutorBackend::Asupersync,
    ));
    list
}

fn run_drain_for_backend(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &'static str,
    lane: RuntimeLane,
    backend: TaskExecutorBackend,
) {
    let mut program = build_program(lane, backend);
    let mut iter_latencies = Vec::new();
    let start_all = Instant::now();
    let mut run_iters = |iters: u64| -> Duration {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            program
                .enqueue_headless_event(Event::Key(KeyEvent::new(KeyCode::Char('d'))))
                .expect("enqueue 50 tasks");
            let t0 = Instant::now();
            program
                .drain_headless_tasks(Duration::from_secs(5))
                .expect("drain 50 tasks");
            let elapsed = t0.elapsed();
            iter_latencies.push(elapsed.as_micros() as u64);
            total += elapsed;
        }
        total
    };
    match name {
        "spawned" => {
            group.bench_function("spawned", |b| b.iter_custom(&mut run_iters));
        }
        "effect_queue" => {
            group.bench_function("effect_queue", |b| b.iter_custom(&mut run_iters));
        }
        _ => {
            group.bench_function("asupersync", |b| b.iter_custom(&mut run_iters));
        }
    }
    emit_summary(
        "runtime/effect_queue_drain",
        name,
        &mut iter_latencies,
        start_all.elapsed().as_secs_f64() * 1000.0,
    );
}

fn bench_effect_queue_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/effect_queue_drain");
    for (name, lane, backend) in backends_to_bench() {
        run_drain_for_backend(&mut group, name, lane, backend);
    }
    group.finish();
}

fn bench_task_burst_200(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/task_burst_200");
    group.sample_size(10);
    for (name, lane, backend) in backends_to_bench() {
        let mut program = build_program(lane, backend);
        let mut burst_latencies = Vec::new();
        let start_all = Instant::now();
        let mut step = || {
            let start = Instant::now();
            program
                .step_headless_event(
                    Event::Key(KeyEvent::new(KeyCode::Char('b'))),
                    Duration::from_secs(5),
                )
                .expect("enqueue and drain 200 burst tasks");
            burst_latencies.push(start.elapsed().as_micros() as u64);
        };
        match name {
            "spawned" => {
                group.bench_function("spawned", |b| b.iter(&mut step));
            }
            "effect_queue" => {
                group.bench_function("effect_queue", |b| b.iter(&mut step));
            }
            _ => {
                group.bench_function("asupersync", |b| b.iter(&mut step));
            }
        }
        let total_ms = start_all.elapsed().as_secs_f64() * 1000.0;
        emit_summary(
            "runtime/task_burst_200",
            name,
            &mut burst_latencies,
            total_ms,
        );
    }
    group.finish();
}

fn bench_pollers_10x10ms(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime/pollers_10x10ms");
    group.sample_size(10);
    for (name, lane, backend) in backends_to_bench() {
        let mut program = build_program(lane, backend);
        let start_all = Instant::now();
        let mut step = || {
            program
                .step_headless_event(
                    Event::Key(KeyEvent::new(KeyCode::Char('p'))),
                    Duration::from_secs(5),
                )
                .expect("echo under 10x10ms pollers");
        };
        match name {
            "spawned" => {
                group.bench_function("spawned", |b| b.iter(&mut step));
            }
            "effect_queue" => {
                group.bench_function("effect_queue", |b| b.iter(&mut step));
            }
            _ => {
                group.bench_function("asupersync", |b| b.iter(&mut step));
            }
        }
        let total_ms = start_all.elapsed().as_secs_f64() * 1000.0;
        emit_summary(
            "runtime/pollers_10x10ms",
            name,
            &mut program.model_mut().echo_latencies_us,
            total_ms,
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_effect_queue_drain,
    bench_task_burst_200,
    bench_pollers_10x10ms
);
criterion_main!(benches);
