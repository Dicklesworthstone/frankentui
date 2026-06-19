//! Advanced host API compatibility harness — marker / viewport-anchor subsystem
//! (bd-2vr05.13.6).
//!
//! This is the marker arm of the cross-subsystem advanced-API compatibility
//! harness. It drives the **production** host-driven runner (`ftui_web::
//! step_program::StepProgram`, the in-tree home of the bd-2vr05.13.4 marker /
//! viewport-stable-anchor primitives) and asserts the deterministic baseline
//! reset + full-repaint boundary markers that a geometry transition (resize,
//! fit, DPR/zoom) emits — the signal a host needs to keep terminal-integrated
//! decorations viewport-stable across reflow.
//!
//! Every cell prints one structured `FTUI_ADVANCED_API_COMPAT ...` JSONL line
//! (subsystem-tagged, with a correlation id) so the aggregator
//! (`scripts/frankenterm_js_advanced_api_compat.sh`) folds it into a single
//! compatibility manifest alongside the parser-hook and runtime-option arms.
//!
//! Determinism: the host advances all time and geometry explicitly; there is no
//! wall-clock or randomness, so the emitted marker stream is reproducible.
#![forbid(unsafe_code)]

use ftui_core::event::Event;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_web::step_program::StepProgram;

// ── Minimal host-driven model ────────────────────────────────────────────────

#[derive(Default)]
struct Canvas {
    ticks: u64,
}

#[derive(Debug, Clone, Copy)]
enum Msg {
    Tick,
    Noop,
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        match event {
            Event::Tick => Self::Tick,
            _ => Self::Noop,
        }
    }
}

impl Model for Canvas {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        if let Msg::Tick = msg {
            self.ticks += 1;
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("ticks={}", self.ticks);
        for (i, ch) in text.chars().enumerate() {
            if (i as u16) >= frame.width() {
                break;
            }
            frame.buffer.set_raw(i as u16, 0, Cell::from_char(ch));
        }
    }
}

/// Emit one structured compatibility-harness cell.
fn emit(scenario: &str, case: &str, correlation_id: &str, passed: bool, extra: &str) {
    println!(
        "FTUI_ADVANCED_API_COMPAT {{\"subsystem\":\"markers\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},{extra}}}"
    );
}

/// Count the two geometry-transition marker kinds in a presenter log batch.
fn count_markers(logs: &[String]) -> (usize, usize) {
    let reset = logs
        .iter()
        .filter(|l| l.contains("\"event\":\"diff_baseline_reset\""))
        .count();
    let repaint = logs
        .iter()
        .filter(|l| l.contains("\"event\":\"full_repaint_boundary\""))
        .count();
    (reset, repaint)
}

fn boot() -> StepProgram<Canvas> {
    let mut prog = StepProgram::new(Canvas::default(), 80, 24);
    prog.init().expect("init");
    // Drain the initial-frame outputs so subsequent assertions only see the
    // markers produced by the transition under test.
    let _ = prog.take_outputs();
    prog
}

// ── Scenarios ────────────────────────────────────────────────────────────────

/// A dimension-changing resize emits exactly one baseline-reset and one
/// full-repaint-boundary marker, with the from/to geometry recorded.
#[test]
fn resize_emits_baseline_and_repaint_markers() {
    let mut prog = boot();
    prog.resize(120, 40);
    prog.step().expect("step");
    let outputs = prog.take_outputs();
    let (reset, repaint) = count_markers(&outputs.logs);

    assert_eq!(reset, 1, "exactly one baseline-reset marker");
    assert_eq!(repaint, 1, "exactly one full-repaint boundary marker");
    assert!(
        outputs.last_full_repaint_hint,
        "the present after a geometry transition must hint full repaint"
    );
    // The marker payload records the from/to geometry for host anchoring.
    assert!(
        outputs
            .logs
            .iter()
            .any(|l| l.contains("\"to_cols\":120") && l.contains("\"to_rows\":40")),
        "marker must record the new geometry"
    );

    emit(
        "resize_transition",
        "80x24_to_120x40",
        "mk-resize",
        reset == 1 && repaint == 1,
        &format!(
            "\"baseline_reset\":{reset},\"full_repaint_boundary\":{repaint},\"full_repaint_hint\":{}",
            outputs.last_full_repaint_hint
        ),
    );
}

/// A same-dimension resize signal (host fit / DPR / zoom) still forces a
/// baseline reset — anchors must survive a re-layout even when cols/rows are
/// numerically unchanged.
#[test]
fn same_dimension_resize_still_resets_baseline() {
    let mut prog = boot();
    prog.resize(80, 24); // identical dimensions, but a real transition signal
    prog.step().expect("step");
    let outputs = prog.take_outputs();
    let (reset, repaint) = count_markers(&outputs.logs);

    assert_eq!(reset, 1, "fit/DPR transition still resets the baseline");
    assert_eq!(
        repaint, 1,
        "fit/DPR transition still forces a repaint boundary"
    );

    emit(
        "fit_dpr_transition",
        "80x24_to_80x24",
        "mk-samegeom",
        reset == 1,
        &format!("\"baseline_reset\":{reset},\"full_repaint_boundary\":{repaint}"),
    );
}

/// A plain tick (no geometry transition) emits no markers — the signal is not
/// spuriously raised.
#[test]
fn tick_without_resize_emits_no_markers() {
    let mut prog = boot();
    prog.push_event(Event::Tick);
    prog.step().expect("step");
    let outputs = prog.take_outputs();
    let (reset, repaint) = count_markers(&outputs.logs);

    assert_eq!(reset, 0, "no baseline reset without a transition");
    assert_eq!(repaint, 0, "no repaint boundary without a transition");

    emit(
        "no_transition",
        "tick_only",
        "mk-none",
        reset == 0 && repaint == 0,
        &format!("\"baseline_reset\":{reset},\"full_repaint_boundary\":{repaint}"),
    );
}

/// Determinism: the same transition produces a byte-identical marker stream.
#[test]
fn marker_stream_is_deterministic() {
    let render = || {
        let mut prog = boot();
        prog.resize(100, 30);
        prog.step().expect("step");
        let mut markers: Vec<String> = prog
            .take_outputs()
            .logs
            .into_iter()
            .filter(|l| l.contains("baseline_reset") || l.contains("full_repaint_boundary"))
            .collect();
        markers.sort();
        markers
    };
    assert_eq!(render(), render(), "marker stream must be deterministic");

    emit(
        "determinism",
        "100x30_repeat",
        "mk-det",
        true,
        "\"reproducible\":true",
    );
}
