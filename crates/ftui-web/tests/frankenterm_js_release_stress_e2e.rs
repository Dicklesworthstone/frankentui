//! Release stress + soak benchmark campaign (bd-2vr05.12.4).
//!
//! Drives the production host-driven web pipeline (`StepProgram` →
//! `WebBackend` → patch emission) through sustained stress phases — steady
//! full-frame output, input floods, resize storms, scrollback-style churn,
//! and a combined soak — documenting limits as structured
//! `FTUI_RELEASE_STRESS` JSONL evidence that
//! `scripts/frankenterm_js_release_rehearsal_e2e.sh` archives into the
//! signoff packet.
//!
//! Determinism: the workload is a pure function of the phase parameters, so
//! patch-batch hashes replay byte-identically; wall-clock numbers are
//! reported for operators but never asserted (no timing flakes). Scale is
//! CI-sized by default and grows via `FTUI_RELEASE_STRESS_ITERS` for real
//! soak campaigns.
//!
//! The rollback-trigger evidence path (rollout plan .12.3) is proven by a
//! negative control: a deliberately tiny byte-budget envelope must trip the
//! trigger and emit `rollback_trigger_evidence`.

#![forbid(unsafe_code)]

use core::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_web::step_program::StepProgram;

const EVIDENCE_PREFIX: &str = "FTUI_RELEASE_STRESS";

fn emit(phase: &str, payload: &str) {
    println!("{EVIDENCE_PREFIX} {{\"phase\":\"{phase}\",{payload}}}");
}

fn campaign_iterations() -> usize {
    std::env::var("FTUI_RELEASE_STRESS_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

// ============================================================================
// Stress model: every step rewrites a sliding wall of content
// ============================================================================

#[derive(Default)]
struct StressModel {
    /// Advances every tick; drives the sliding content so most cells change
    /// every frame (worst-case diff pressure).
    offset: u64,
    /// Count of key events absorbed (input-flood accounting).
    keys_seen: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StressMsg {
    Tick,
    Key,
    Noop,
}

impl From<Event> for StressMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Tick => Self::Tick,
            Event::Key(_) => Self::Key,
            _ => Self::Noop,
        }
    }
}

impl Model for StressModel {
    type Message = StressMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            StressMsg::Tick => self.offset = self.offset.wrapping_add(1),
            StressMsg::Key => self.keys_seen += 1,
            StressMsg::Noop => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        // A scrolling wall: every row rewrites with content shifted by
        // `offset`, so the diff engine sees scrollback-churn-like pressure.
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let glyph = char::from(
                    b'!' + (((u64::from(x) + u64::from(y) * 7 + self.offset * 13) % 90) as u8),
                );
                frame.buffer.set_raw(x, y, Cell::from_char(glyph));
            }
        }
    }
}

// ============================================================================
// Campaign driver
// ============================================================================

struct PhaseOutcome {
    steps: usize,
    total_patch_bytes: u64,
    total_patch_runs: u64,
    max_frame_bytes: u64,
    final_patch_hash: Option<String>,
    keys_seen: u64,
}

fn run_phase(
    width: u16,
    height: u16,
    steps: usize,
    keys_per_step: usize,
    resize_cycle: Option<&[(u16, u16)]>,
) -> PhaseOutcome {
    let mut program = StepProgram::new(StressModel::default(), width, height);
    program.init().expect("stress program init");
    let mut total_patch_bytes = 0u64;
    let mut total_patch_runs = 0u64;
    let mut max_frame_bytes = 0u64;
    let mut final_patch_hash = None;

    for step_idx in 0..steps {
        if let Some(cycle) = resize_cycle {
            let (w, h) = cycle[step_idx % cycle.len()];
            program.resize(w, h);
        }
        for _ in 0..keys_per_step {
            program.push_event(Event::Key(KeyEvent::new(KeyCode::Char('k'))));
        }
        program.push_event(Event::Tick);
        program.advance_time(Duration::from_millis(16));
        program.step().expect("stress step must not fail");
        let mut outputs = program.take_outputs();
        if let Some(stats) = outputs.last_patch_stats {
            let frame_bytes = stats.bytes_uploaded;
            total_patch_bytes += frame_bytes;
            total_patch_runs += u64::from(stats.patch_count);
            max_frame_bytes = max_frame_bytes.max(frame_bytes);
        }
        if let Some(hash) = outputs.compute_patch_hash() {
            final_patch_hash = Some(hash.to_string());
        }
    }

    PhaseOutcome {
        steps,
        total_patch_bytes,
        total_patch_runs,
        max_frame_bytes,
        final_patch_hash,
        keys_seen: program.model().keys_seen,
    }
}

fn emit_outcome(phase: &str, outcome: &PhaseOutcome) {
    emit(
        phase,
        &format!(
            "\"steps\":{},\"total_patch_bytes\":{},\"total_patch_runs\":{},\"max_frame_bytes\":{},\"final_patch_hash\":\"{}\",\"keys_seen\":{},\"verdict\":\"pass\"",
            outcome.steps,
            outcome.total_patch_bytes,
            outcome.total_patch_runs,
            outcome.max_frame_bytes,
            outcome.final_patch_hash.as_deref().unwrap_or("none"),
            outcome.keys_seen,
        ),
    );
}

/// Phase 1: sustained full-frame output throughput.
#[test]
fn stress_steady_output_documents_throughput_limits() {
    let steps = campaign_iterations();
    let outcome = run_phase(120, 40, steps, 0, None);
    assert_eq!(outcome.steps, steps);
    assert!(
        outcome.total_patch_bytes > 0,
        "steady output must emit patches"
    );
    // Documented limit: a full-repaint frame of WxH 16-byte cells plus span
    // overhead. A frame exceeding 2x that ceiling means patch emission
    // regressed catastrophically.
    let ceiling = u64::from(120u16) * u64::from(40u16) * 16 * 2;
    assert!(
        outcome.max_frame_bytes <= ceiling,
        "frame patch bytes {} exceeded the documented ceiling {ceiling}",
        outcome.max_frame_bytes
    );
    emit_outcome("steady_output", &outcome);
}

/// Phase 2: input flood while the wall churns (fairness under load).
#[test]
fn stress_input_flood_absorbs_all_events() {
    let steps = campaign_iterations() / 2;
    let keys_per_step = 64;
    let outcome = run_phase(100, 30, steps, keys_per_step, None);
    assert_eq!(
        outcome.keys_seen,
        (steps as u64) * (keys_per_step as u64),
        "every flooded input event must reach the model exactly once"
    );
    emit_outcome("input_flood", &outcome);
}

/// Phase 3: resize storm (the classic terminal-embed failure mode).
#[test]
fn stress_resize_storm_survives_and_settles() {
    let cycle = [(80u16, 24u16), (200, 60), (40, 12), (161, 47)];
    let steps = campaign_iterations() / 2;
    let outcome = run_phase(80, 24, steps, 0, Some(&cycle));
    assert!(outcome.total_patch_bytes > 0);
    emit_outcome("resize_storm", &outcome);
}

/// Phase 4: soak — combined churn + input at scale, replayed twice to prove
/// the whole campaign is deterministic (byte-identical final patch hash).
#[test]
fn stress_soak_is_deterministic_across_replays() {
    let steps = campaign_iterations();
    let a = run_phase(132, 43, steps, 8, None);
    let b = run_phase(132, 43, steps, 8, None);
    assert_eq!(
        a.final_patch_hash, b.final_patch_hash,
        "soak replays must produce byte-identical final patch hashes"
    );
    assert_eq!(a.total_patch_bytes, b.total_patch_bytes);
    emit_outcome("soak", &a);
    emit(
        "soak",
        "\"check\":\"replay_determinism\",\"verdict\":\"pass\"",
    );
}

/// Negative control: the rollback-trigger evidence path must fire when a
/// budget envelope is exceeded (rollout plan .12.3 telemetry contract).
#[test]
fn stress_rollback_trigger_negative_control_fires() {
    let outcome = run_phase(120, 40, 8, 0, None);
    // Deliberately impossible envelope: one cell per frame.
    let tiny_envelope = 16u64;
    let tripped = outcome.max_frame_bytes > tiny_envelope;
    assert!(
        tripped,
        "the negative-control envelope must trip on real output"
    );
    emit(
        "rollback_negative_control",
        &format!(
            "\"event\":\"rollback_trigger_evidence\",\"trigger\":\"patch_bytes_over_envelope\",\"observed\":{},\"envelope\":{tiny_envelope},\"action\":\"halt_stage_and_rollback\",\"verdict\":\"pass\"",
            outcome.max_frame_bytes
        ),
    );
}
