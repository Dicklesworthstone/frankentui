#![forbid(unsafe_code)]

//! End-to-end integration tests for the validation pipeline:
//! shadow-run comparison, benchmark gate enforcement, and lab determinism.
//!
//! These tests prove the full flow from model creation through shadow
//! comparison and performance gating, exercising the infrastructure
//! built for bd-cznw0.

use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_harness::benchmark_gate::{BenchmarkGate, Measurement, MetricVerdict, Threshold};
use ftui_harness::lab_integration::{Lab, LabConfig};
use ftui_harness::shadow_run::{ShadowRun, ShadowRunConfig, ShadowVerdict};
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_widgets::Widget;
use ftui_widgets::paragraph::Paragraph;

// ============================================================================
// Test Models
// ============================================================================

/// Deterministic counter model for shadow-run testing.
struct CounterApp {
    count: u64,
}

#[derive(Debug, Clone)]
enum CounterMsg {
    Increment,
    Quit,
}

impl From<Event> for CounterMsg {
    fn from(e: Event) -> Self {
        match e {
            Event::Tick => CounterMsg::Increment,
            _ => CounterMsg::Quit,
        }
    }
}

impl Model for CounterApp {
    type Message = CounterMsg;

    fn update(&mut self, msg: CounterMsg) -> Cmd<CounterMsg> {
        match msg {
            CounterMsg::Increment => {
                self.count += 1;
                Cmd::none()
            }
            CounterMsg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("Counter: {} ticks", self.count);
        let area = Rect::new(0, 0, frame.width(), 1);
        Paragraph::new(text).render(area, frame);
    }
}

/// Model with non-trivial rendering for stress testing.
struct MultiLineApp {
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
enum MultiMsg {
    AddLine,
    Quit,
}

impl From<Event> for MultiMsg {
    fn from(e: Event) -> Self {
        match e {
            Event::Tick => MultiMsg::AddLine,
            _ => MultiMsg::Quit,
        }
    }
}

impl Model for MultiLineApp {
    type Message = MultiMsg;

    fn update(&mut self, msg: MultiMsg) -> Cmd<MultiMsg> {
        match msg {
            MultiMsg::AddLine => {
                let n = self.lines.len();
                self.lines.push(format!("Line {n}: deterministic content"));
                Cmd::none()
            }
            MultiMsg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        for (i, line) in self.lines.iter().enumerate() {
            if i as u16 >= frame.height() {
                break;
            }
            let area = Rect::new(0, i as u16, frame.width(), 1);
            Paragraph::new(line.as_str()).render(area, frame);
        }
    }
}

// ============================================================================
// Shadow-run E2E tests
// ============================================================================

#[test]
fn shadow_run_deterministic_counter_matches() {
    let config = ShadowRunConfig::new("e2e_shadow", "counter_determinism", 42)
        .viewport(80, 24)
        .time_step_ms(16);

    let result = ShadowRun::assert_match(
        config,
        || CounterApp { count: 0 },
        |session| {
            session.init();
            for _ in 0..10 {
                session.tick();
                session.capture_frame();
            }
        },
    );

    assert_eq!(result.verdict, ShadowVerdict::Match);
    assert_eq!(result.frames_compared, 10);
    assert_eq!(result.seed, 42);
}

#[test]
fn shadow_run_multiline_model_matches() {
    let config = ShadowRunConfig::new("e2e_shadow", "multiline_determinism", 7)
        .viewport(60, 20)
        .lane_labels("lane_a", "lane_b");

    let result = ShadowRun::compare(
        config,
        || MultiLineApp {
            lines: vec!["Initial".to_string()],
        },
        |session| {
            session.init();
            session.capture_frame();
            for _ in 0..5 {
                session.tick();
                session.capture_frame();
            }
        },
    );

    assert_eq!(result.verdict, ShadowVerdict::Match);
    assert_eq!(result.frames_compared, 6);
    assert_eq!(result.baseline_label, "lane_a");
    assert_eq!(result.candidate_label, "lane_b");
    assert!((result.match_ratio() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn shadow_run_different_seeds_produce_same_output_for_deterministic_model() {
    // Even with different seed values, a purely deterministic model
    // (one that doesn't use the seed) should produce identical frames.
    let config = ShadowRunConfig::new("e2e_shadow", "seed_independence", 1);

    let result = ShadowRun::compare(
        config,
        || CounterApp { count: 0 },
        |session| {
            session.init();
            session.tick();
            session.tick();
            session.capture_frame();
        },
    );

    assert_eq!(result.verdict, ShadowVerdict::Match);
}

// ============================================================================
// Lab determinism E2E tests
// ============================================================================

#[test]
fn lab_determinism_proof_counter() {
    let config = LabConfig::new("e2e_lab", "counter_proof", 42)
        .viewport(80, 24)
        .time_step_ms(16);

    let output = Lab::assert_deterministic_with(
        config,
        || CounterApp { count: 0 },
        |session| {
            session.init();
            for _ in 0..20 {
                session.tick();
                session.capture_frame();
            }
        },
    );

    assert_eq!(output.frame_count, 20);
    assert_eq!(output.anomaly_count, 0);
}

#[test]
fn lab_record_and_replay_matches() {
    let config = LabConfig::new("e2e_lab", "record_replay", 99)
        .viewport(80, 24)
        .time_step_ms(16);

    let recording = Lab::record(config, CounterApp { count: 0 }, |session| {
        session.init();
        for _ in 0..5 {
            session.tick();
            session.capture_frame();
        }
    });

    assert_eq!(recording.frame_records.len(), 5);

    let replay_result = Lab::replay(&recording, CounterApp { count: 0 }, |session| {
        session.init();
        for _ in 0..5 {
            session.tick();
            session.capture_frame();
        }
    });

    assert!(replay_result.matched);
    assert_eq!(replay_result.frames_compared, 5);
    assert!(replay_result.first_divergence.is_none());
}

// ============================================================================
// Benchmark gate E2E tests
// ============================================================================

#[test]
fn benchmark_gate_pass_with_realistic_metrics() {
    let gate = BenchmarkGate::new("e2e_perf_gate")
        .threshold(Threshold::new("frame_render_p99_us", 2000.0).tolerance_pct(10.0))
        .threshold(Threshold::new("diff_compute_p99_us", 500.0).tolerance_pct(15.0))
        .threshold(Threshold::new("present_p99_us", 300.0).tolerance_pct(5.0));

    let measurements = vec![
        Measurement::new("frame_render_p99_us", 1750.0).unit("μs"),
        Measurement::new("diff_compute_p99_us", 420.0).unit("μs"),
        Measurement::new("present_p99_us", 280.0).unit("μs"),
        Measurement::new("ansi_bytes_per_frame", 4096.0).unit("bytes"),
    ];

    let result = gate.evaluate(&measurements);
    assert!(result.passed(), "gate should pass: {}", result.summary());
    assert_eq!(result.pass_count, 3);
    assert_eq!(result.unchecked_count, 1); // ansi_bytes_per_frame has no threshold
}

#[test]
fn benchmark_gate_fail_on_regression() {
    let gate = BenchmarkGate::new("e2e_regression_gate")
        .threshold(Threshold::new("frame_render_p99_us", 2000.0).tolerance_pct(10.0))
        .threshold(Threshold::new("diff_compute_p99_us", 500.0).tolerance_pct(5.0));

    let measurements = vec![
        Measurement::new("frame_render_p99_us", 1900.0).unit("μs"), // OK: within tolerance
        Measurement::new("diff_compute_p99_us", 600.0).unit("μs"),  // FAIL: 20% over budget
    ];

    let result = gate.evaluate(&measurements);
    assert!(!result.passed(), "gate should fail on regression");
    assert_eq!(result.fail_count, 1);

    let failures = result.failures();
    assert_eq!(failures[0].metric, "diff_compute_p99_us");
    assert_eq!(failures[0].verdict, MetricVerdict::Fail);
}

#[test]
fn benchmark_gate_load_from_json_baseline() {
    let baseline_json = r#"{
        "frame_render_p99_us": { "budget": 2000.0, "tolerance_pct": 10.0 },
        "diff_compute_p99_us": { "budget": 500.0, "tolerance_pct": 15.0 },
        "present_p99_us": { "budget": 300.0 }
    }"#;

    let gate = BenchmarkGate::load_json("e2e_json_gate", baseline_json)
        .expect("baseline JSON should parse");

    let measurements = vec![
        Measurement::new("frame_render_p99_us", 2100.0), // Within 10% tolerance
        Measurement::new("diff_compute_p99_us", 550.0),  // Within 15% tolerance
        Measurement::new("present_p99_us", 290.0),       // Under budget
    ];

    let result = gate.evaluate(&measurements);
    assert!(result.passed(), "gate should pass: {}", result.summary());
}

// ============================================================================
// Baseline JSON integration
// ============================================================================

#[test]
fn benchmark_gate_loads_real_baseline_json() {
    let baseline_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/baseline.json");
    let json = std::fs::read_to_string(baseline_path).expect("baseline.json should exist");

    let gate = BenchmarkGate::load_baseline_json("real_baseline_p99", &json, "p99_ns")
        .expect("real baseline.json should parse");

    // Synthetic input exercises the gate contract, not runtime performance.
    // Every required metric in the real baseline must be represented.
    let measurements = vec![
        Measurement::new("frame_render", 1_000_000.0).unit("ns"),
        Measurement::new("layout_computation", 10_000.0).unit("ns"),
        Measurement::new("diff_strategy", 100_000.0).unit("ns"),
        Measurement::new("diff_strategy_large", 400_000.0).unit("ns"),
        Measurement::new("widget_render_block", 25_000.0).unit("ns"),
        Measurement::new("widget_render_table", 60_000.0).unit("ns"),
        Measurement::new("ansi_emit", 200_000.0).unit("ns"),
        Measurement::new("buffer_new_80x24", 10_000.0).unit("ns"),
        Measurement::new("buffer_new_200x60", 40_000.0).unit("ns"),
        Measurement::new("cell_bits_eq", 5.0).unit("ns"),
        Measurement::new("runtime_shutdown_latency", 2_500_000.0).unit("ns"),
        Measurement::new("runtime_first_frame", 5_000_000.0).unit("ns"),
        Measurement::new("runtime_command_roundtrip", 250_000.0).unit("ns"),
        Measurement::new("runtime_effect_queue_drain", 1_000_000.0).unit("ns"),
    ];

    let result = gate.evaluate(&measurements);
    assert!(
        result.passed(),
        "all under-budget measurements should pass: {}",
        result.summary()
    );
    assert_eq!(result.pass_count, 14);

    // The old ten-metric fixture silently omitted four required runtime
    // measurements; it must no longer certify the full baseline.
    let incomplete = gate.evaluate(&measurements[..10]);
    assert!(!incomplete.passed());
    assert_eq!(incomplete.validation_errors.len(), 4);
}

// ============================================================================
// Combined flow: shadow-run + benchmark gate
// ============================================================================

#[test]
fn combined_shadow_and_gate_validation_flow() {
    // Step 1: Shadow-run proves rendering equivalence
    let shadow_config = ShadowRunConfig::new("e2e_combined", "full_flow", 42)
        .viewport(80, 24)
        .lane_labels("threading", "asupersync");

    let shadow_result = ShadowRun::compare(
        shadow_config,
        || CounterApp { count: 0 },
        |session| {
            session.init();
            for _ in 0..10 {
                session.tick();
                session.capture_frame();
            }
        },
    );

    assert_eq!(
        shadow_result.verdict,
        ShadowVerdict::Match,
        "shadow comparison must match before gating"
    );

    // Step 2: Benchmark gate enforces performance thresholds
    // The gate checks that measured values stay UNDER budget. Use synthetic
    // latency-like metrics derived from the shadow run to demonstrate the flow.
    let gate = BenchmarkGate::new("e2e_combined_gate")
        .threshold(Threshold::new("render_time_us", 5000.0).tolerance_pct(10.0))
        .threshold(Threshold::new("diverged_frames", 1.0));

    let measurements = vec![
        Measurement::new("render_time_us", 1200.0).unit("μs"),
        Measurement::new("diverged_frames", shadow_result.diverged_count() as f64),
    ];

    let gate_result = gate.evaluate(&measurements);
    assert!(
        gate_result.passed(),
        "benchmark gate should pass: {}",
        gate_result.summary()
    );
}
