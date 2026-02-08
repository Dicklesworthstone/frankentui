#![forbid(unsafe_code)]

//! Deterministic session recording and replay for WASM (bd-lff4p.3.7).
//!
//! Provides [`SessionRecorder`] for recording input events, time steps, and
//! resize events during a WASM session, and [`replay`] for replaying them
//! through a fresh model to verify that frame checksums match exactly.
//!
//! # Design
//!
//! Follows the golden-trace-v1 schema defined in
//! `docs/spec/frankenterm-golden-trace-format.md`:
//!
//! - **Header**: seed, initial dimensions, capability profile.
//! - **Input**: timestamped terminal events (key, mouse, paste, etc.).
//! - **Resize**: terminal resize events.
//! - **Tick**: explicit time advancement events.
//! - **Frame**: frame checkpoints with FNV-1a checksums and chaining.
//! - **Summary**: total frames and final checksum chain.
//!
//! # Determinism contract
//!
//! Given identical recorded inputs and the same model implementation, replay
//! **must** produce identical frame checksums on the same build. This is
//! guaranteed by:
//!
//! 1. Host-driven clock (no `Instant::now()` — time only advances via explicit
//!    tick records).
//! 2. Host-driven events (no polling — events are replayed from the trace).
//! 3. Deterministic rendering (same model state → same buffer → same checksum).
//!
//! # Example
//!
//! ```ignore
//! let mut recorder = SessionRecorder::new(MyModel::default(), 80, 24, /*seed=*/0);
//! recorder.init().unwrap();
//!
//! recorder.push_event(0, key_event('+'));
//! recorder.advance_time(16_000_000, Duration::from_millis(16));
//! recorder.step().unwrap();
//!
//! let trace = recorder.finish();
//! let result = replay(MyModel::default(), &trace).unwrap();
//! assert!(result.ok());
//! ```

use core::time::Duration;

use ftui_core::event::Event;
use ftui_runtime::render_trace::checksum_buffer;

use crate::WebBackendError;
use crate::step_program::{StepProgram, StepResult};

/// Schema version for session traces.
pub const SCHEMA_VERSION: &str = "golden-trace-v1";

// FNV-1a constants — identical to ftui-runtime/src/render_trace.rs.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a64_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fnv1a64_u64(hash: u64, v: u64) -> u64 {
    fnv1a64_bytes(hash, &v.to_le_bytes())
}

fn fnv1a64_pair(prev: u64, next: u64) -> u64 {
    let hash = FNV_OFFSET_BASIS;
    let hash = fnv1a64_u64(hash, prev);
    fnv1a64_u64(hash, next)
}

/// A single record in a session trace.
#[derive(Debug, Clone, PartialEq)]
pub enum TraceRecord {
    /// Session header (must be first).
    Header {
        seed: u64,
        cols: u16,
        rows: u16,
        profile: String,
    },
    /// An input event at a specific timestamp.
    Input { ts_ns: u64, event: Event },
    /// Terminal resize at a specific timestamp.
    Resize { ts_ns: u64, cols: u16, rows: u16 },
    /// Explicit time advancement.
    Tick { ts_ns: u64 },
    /// Frame checkpoint with checksum.
    Frame {
        frame_idx: u64,
        ts_ns: u64,
        checksum: u64,
        checksum_chain: u64,
    },
    /// Trace summary (must be last).
    Summary {
        total_frames: u64,
        final_checksum_chain: u64,
    },
}

/// A complete recorded session trace.
#[derive(Debug, Clone)]
pub struct SessionTrace {
    pub records: Vec<TraceRecord>,
}

impl SessionTrace {
    /// Number of frame checkpoints in the trace.
    pub fn frame_count(&self) -> u64 {
        self.records
            .iter()
            .filter(|r| matches!(r, TraceRecord::Frame { .. }))
            .count() as u64
    }

    /// Extract the final checksum chain from the summary record.
    pub fn final_checksum_chain(&self) -> Option<u64> {
        self.records.iter().rev().find_map(|r| match r {
            TraceRecord::Summary {
                final_checksum_chain,
                ..
            } => Some(*final_checksum_chain),
            _ => None,
        })
    }
}

/// Records a WASM session for deterministic replay.
///
/// Wraps a [`StepProgram`] and intercepts all input operations, recording
/// them as [`TraceRecord`]s. Frame checksums are computed after each render
/// using the same FNV-1a algorithm as the render trace system.
pub struct SessionRecorder<M: ftui_runtime::program::Model> {
    program: StepProgram<M>,
    records: Vec<TraceRecord>,
    checksum_chain: u64,
    current_ts_ns: u64,
}

impl<M: ftui_runtime::program::Model> SessionRecorder<M> {
    /// Create a new recorder with the given model, initial size, and seed.
    #[must_use]
    pub fn new(model: M, width: u16, height: u16, seed: u64) -> Self {
        let program = StepProgram::new(model, width, height);
        let records = vec![TraceRecord::Header {
            seed,
            cols: width,
            rows: height,
            profile: "modern".to_string(),
        }];
        Self {
            program,
            records,
            checksum_chain: 0,
            current_ts_ns: 0,
        }
    }

    /// Initialize the model and record the first frame checkpoint.
    pub fn init(&mut self) -> Result<(), WebBackendError> {
        self.program.init()?;
        self.record_frame();
        Ok(())
    }

    /// Record an input event at the given timestamp (nanoseconds since start).
    pub fn push_event(&mut self, ts_ns: u64, event: Event) {
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Input {
            ts_ns,
            event: event.clone(),
        });
        self.program.push_event(event);
    }

    /// Record a resize at the given timestamp.
    pub fn resize(&mut self, ts_ns: u64, width: u16, height: u16) {
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Resize {
            ts_ns,
            cols: width,
            rows: height,
        });
        self.program.resize(width, height);
    }

    /// Record a time advancement (tick) at the given timestamp.
    pub fn advance_time(&mut self, ts_ns: u64, dt: Duration) {
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Tick { ts_ns });
        self.program.advance_time(dt);
    }

    /// Process one step and record a frame checkpoint if rendered.
    pub fn step(&mut self) -> Result<StepResult, WebBackendError> {
        let result = self.program.step()?;
        if result.rendered {
            self.record_frame();
        }
        Ok(result)
    }

    /// Finish recording and return the completed trace.
    pub fn finish(mut self) -> SessionTrace {
        let total_frames = self
            .records
            .iter()
            .filter(|r| matches!(r, TraceRecord::Frame { .. }))
            .count() as u64;
        self.records.push(TraceRecord::Summary {
            total_frames,
            final_checksum_chain: self.checksum_chain,
        });
        SessionTrace {
            records: self.records,
        }
    }

    /// Access the underlying program.
    pub fn program(&self) -> &StepProgram<M> {
        &self.program
    }

    /// Mutably access the underlying program.
    pub fn program_mut(&mut self) -> &mut StepProgram<M> {
        &mut self.program
    }

    fn record_frame(&mut self) {
        let outputs = self.program.outputs();
        if let Some(buf) = &outputs.last_buffer {
            let checksum = checksum_buffer(buf, self.program.pool());
            let chain = fnv1a64_pair(self.checksum_chain, checksum);
            self.records.push(TraceRecord::Frame {
                frame_idx: self.program.frame_idx().saturating_sub(1),
                ts_ns: self.current_ts_ns,
                checksum,
                checksum_chain: chain,
            });
            self.checksum_chain = chain;
        }
    }
}

/// Result of replaying a session trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayResult {
    /// Total frames replayed.
    pub total_frames: u64,
    /// Final checksum chain from replay.
    pub final_checksum_chain: u64,
    /// First frame where a checksum mismatch was detected, if any.
    pub first_mismatch: Option<ReplayMismatch>,
}

impl ReplayResult {
    /// Whether the replay produced identical checksums.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.first_mismatch.is_none()
    }
}

/// Description of a checksum mismatch during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayMismatch {
    /// Frame index where the mismatch occurred.
    pub frame_idx: u64,
    /// Expected checksum from the trace.
    pub expected: u64,
    /// Actual checksum from replay.
    pub actual: u64,
}

/// Errors that can occur during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// The trace is missing a header record.
    MissingHeader,
    /// A backend error occurred during replay.
    Backend(WebBackendError),
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingHeader => write!(f, "trace missing header record"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<WebBackendError> for ReplayError {
    fn from(e: WebBackendError) -> Self {
        Self::Backend(e)
    }
}

/// Replay a recorded session trace through a fresh model.
///
/// Feeds all recorded events, resizes, and ticks through a new
/// [`StepProgram`], stepping only at frame boundaries (matching the
/// original recording cadence). Compares frame checksums against the
/// recorded values.
///
/// Returns [`ReplayResult`] with match/mismatch information.
pub fn replay<M: ftui_runtime::program::Model>(
    model: M,
    trace: &SessionTrace,
) -> Result<ReplayResult, ReplayError> {
    // Extract header.
    let (cols, rows) = trace
        .records
        .first()
        .and_then(|r| match r {
            TraceRecord::Header { cols, rows, .. } => Some((*cols, *rows)),
            _ => None,
        })
        .ok_or(ReplayError::MissingHeader)?;

    let mut program = StepProgram::new(model, cols, rows);
    program.init()?;

    let mut replay_frame_idx: u64 = 0;
    let mut checksum_chain: u64 = 0;
    let mut first_mismatch: Option<ReplayMismatch> = None;

    // Replay by iterating through trace records. Input/Resize/Tick records
    // feed data into the program; Frame records trigger a step and checksum
    // verification. This ensures event batching matches the original session.
    for record in &trace.records {
        match record {
            TraceRecord::Input { event, .. } => {
                program.push_event(event.clone());
            }
            TraceRecord::Resize { cols, rows, .. } => {
                program.resize(*cols, *rows);
            }
            TraceRecord::Tick { ts_ns } => {
                program.set_time(Duration::from_nanos(*ts_ns));
            }
            TraceRecord::Frame {
                frame_idx: expected_idx,
                checksum: expected_checksum,
                ..
            } => {
                // The init frame (frame_idx 0) was already rendered by init().
                // Subsequent frames require a step() call.
                if replay_frame_idx > 0 {
                    program.step()?;
                }

                // Verify checksum.
                let outputs = program.outputs();
                if let Some(buf) = &outputs.last_buffer {
                    let actual = checksum_buffer(buf, program.pool());
                    checksum_chain = fnv1a64_pair(checksum_chain, actual);
                    if actual != *expected_checksum && first_mismatch.is_none() {
                        first_mismatch = Some(ReplayMismatch {
                            frame_idx: *expected_idx,
                            expected: *expected_checksum,
                            actual,
                        });
                    }
                }
                replay_frame_idx += 1;
            }
            TraceRecord::Header { .. } | TraceRecord::Summary { .. } => {}
        }
    }

    Ok(ReplayResult {
        total_frames: replay_frame_idx,
        final_checksum_chain: checksum_chain,
        first_mismatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{
        KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
        PasteEvent,
    };
    use ftui_render::cell::Cell;
    use ftui_render::frame::Frame;
    use ftui_runtime::program::{Cmd, Model};
    use pretty_assertions::assert_eq;

    // ---- Test model (same as step_program tests) ----

    struct Counter {
        value: i32,
    }

    #[derive(Debug)]
    enum CounterMsg {
        Increment,
        Decrement,
        Reset,
        Quit,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('r') => CounterMsg::Reset,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                Event::Tick => CounterMsg::Increment,
                _ => CounterMsg::Increment,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
            Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                CounterMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                CounterMsg::Reset => {
                    self.value = 0;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, frame: &mut Frame) {
            let text = format!("Count: {}", self.value);
            for (i, c) in text.chars().enumerate() {
                if (i as u16) < frame.width() {
                    frame.buffer.set_raw(i as u16, 0, Cell::from_char(c));
                }
            }
        }
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn new_counter(value: i32) -> Counter {
        Counter { value }
    }

    // ---- FNV-1a hash tests ----

    #[test]
    fn fnv1a64_pair_is_deterministic() {
        let a = fnv1a64_pair(0, 1234);
        let b = fnv1a64_pair(0, 1234);
        assert_eq!(a, b);
    }

    #[test]
    fn fnv1a64_pair_differs_for_different_input() {
        assert_ne!(fnv1a64_pair(0, 1), fnv1a64_pair(0, 2));
        assert_ne!(fnv1a64_pair(1, 0), fnv1a64_pair(2, 0));
    }

    // ---- Recorder basic lifecycle ----

    #[test]
    fn recorder_produces_header_and_summary() {
        let mut rec = SessionRecorder::new(new_counter(0), 80, 24, 42);
        rec.init().unwrap();

        let trace = rec.finish();
        assert!(trace.records.len() >= 3); // header + frame + summary

        // First record is header.
        assert!(matches!(
            &trace.records[0],
            TraceRecord::Header {
                seed: 42,
                cols: 80,
                rows: 24,
                ..
            }
        ));

        // Last record is summary.
        assert!(matches!(
            trace.records.last().unwrap(),
            TraceRecord::Summary {
                total_frames: 1,
                ..
            }
        ));
    }

    #[test]
    fn recorder_captures_init_frame() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        let trace = rec.finish();
        let frames: Vec<_> = trace
            .records
            .iter()
            .filter(|r| matches!(r, TraceRecord::Frame { .. }))
            .collect();
        assert_eq!(frames.len(), 1);

        if let TraceRecord::Frame {
            frame_idx,
            checksum,
            ..
        } = &frames[0]
        {
            assert_eq!(*frame_idx, 0);
            assert_ne!(*checksum, 0); // Non-trivial checksum.
        }
    }

    // ---- Record and replay ----

    #[test]
    fn record_replay_identical_checksums() {
        // Record a session.
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('+'));
        rec.push_event(3_000_000, key_event('-'));
        rec.step().unwrap();

        rec.push_event(16_000_000, key_event('+'));
        rec.step().unwrap();

        let trace = rec.finish();
        assert_eq!(trace.frame_count(), 3); // init + 2 steps

        // Replay with a fresh model.
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok(), "replay mismatch: {:?}", result.first_mismatch);
        assert_eq!(result.total_frames, 3);
        assert_eq!(
            result.final_checksum_chain,
            trace.final_checksum_chain().unwrap()
        );
    }

    #[test]
    fn replay_detects_different_initial_state() {
        // Record with counter starting at 0.
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        let trace = rec.finish();

        // Replay with counter starting at 5 — different init state → different checksum.
        let result = replay(new_counter(5), &trace).unwrap();
        assert!(!result.ok());
        assert_eq!(result.first_mismatch.as_ref().unwrap().frame_idx, 0);
    }

    #[test]
    fn replay_detects_divergence_after_events() {
        // Record with normal counter.
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('+'));
        rec.step().unwrap();

        let trace = rec.finish();

        // Replay with a model that starts at 1 instead of 0.
        let result = replay(new_counter(1), &trace).unwrap();
        assert!(!result.ok());
    }

    // ---- Resize recording ----

    #[test]
    fn resize_is_recorded_and_replayed() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.resize(5_000_000, 40, 2);
        rec.step().unwrap();

        let trace = rec.finish();

        // Verify resize record exists.
        assert!(trace.records.iter().any(|r| matches!(
            r,
            TraceRecord::Resize {
                cols: 40,
                rows: 2,
                ..
            }
        )));

        // Replay should match.
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(
            result.ok(),
            "resize replay mismatch: {:?}",
            result.first_mismatch
        );
    }

    // ---- Multiple steps ----

    #[test]
    fn multi_step_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        for i in 0..5 {
            rec.push_event(i * 16_000_000, key_event('+'));
            rec.step().unwrap();
        }

        let trace = rec.finish();
        assert_eq!(trace.frame_count(), 6); // init + 5 steps

        let result = replay(new_counter(0), &trace).unwrap();
        assert!(
            result.ok(),
            "multi-step mismatch: {:?}",
            result.first_mismatch
        );
        assert_eq!(result.total_frames, 6);
    }

    // ---- Quit during session ----

    #[test]
    fn quit_stops_recording() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('q'));
        let result = rec.step().unwrap();
        assert!(!result.running);

        let trace = rec.finish();
        // init frame + no render after quit (quit stops before render).
        assert_eq!(trace.frame_count(), 1);
    }

    // ---- Empty session ----

    #[test]
    fn empty_session_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        let trace = rec.finish();

        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
        assert_eq!(result.total_frames, 1); // Just the init frame.
    }

    // ---- Trace accessors ----

    #[test]
    fn session_trace_frame_count() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'));
        rec.step().unwrap();
        let trace = rec.finish();
        assert_eq!(trace.frame_count(), 2);
    }

    #[test]
    fn session_trace_final_checksum_chain() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        let trace = rec.finish();
        assert!(trace.final_checksum_chain().is_some());
        assert_ne!(trace.final_checksum_chain().unwrap(), 0);
    }

    // ---- Replay error cases ----

    #[test]
    fn replay_missing_header_returns_error() {
        let trace = SessionTrace { records: vec![] };
        let result = replay(new_counter(0), &trace);
        assert!(matches!(result, Err(ReplayError::MissingHeader)));
    }

    #[test]
    fn replay_non_header_first_returns_error() {
        let trace = SessionTrace {
            records: vec![TraceRecord::Tick { ts_ns: 0 }],
        };
        let result = replay(new_counter(0), &trace);
        assert!(matches!(result, Err(ReplayError::MissingHeader)));
    }

    // ---- Determinism: same input → same trace ----

    #[test]
    fn same_inputs_produce_same_trace_checksums() {
        fn record_session() -> SessionTrace {
            let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
            rec.init().unwrap();

            rec.push_event(1_000_000, key_event('+'));
            rec.push_event(2_000_000, key_event('+'));
            rec.push_event(3_000_000, key_event('-'));
            rec.step().unwrap();

            rec.push_event(16_000_000, key_event('+'));
            rec.step().unwrap();

            rec.finish()
        }

        let t1 = record_session();
        let t2 = record_session();
        let t3 = record_session();

        // All traces should have identical frame checksums.
        let checksums = |t: &SessionTrace| -> Vec<u64> {
            t.records
                .iter()
                .filter_map(|r| match r {
                    TraceRecord::Frame { checksum, .. } => Some(*checksum),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(checksums(&t1), checksums(&t2));
        assert_eq!(checksums(&t2), checksums(&t3));
        assert_eq!(t1.final_checksum_chain(), t2.final_checksum_chain());
    }

    // ---- Mouse, paste, and focus events ----

    #[test]
    fn mouse_event_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 5,
            y: 0,
            modifiers: Modifiers::empty(),
        });
        rec.push_event(1_000_000, mouse);
        rec.step().unwrap();

        let trace = rec.finish();
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
    }

    #[test]
    fn paste_event_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        let paste = Event::Paste(PasteEvent::bracketed("hello"));
        rec.push_event(1_000_000, paste);
        rec.step().unwrap();

        let trace = rec.finish();
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
    }

    #[test]
    fn focus_event_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, Event::Focus(true));
        rec.push_event(2_000_000, Event::Focus(false));
        rec.step().unwrap();

        let trace = rec.finish();
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
    }

    // ---- Checksum chain integrity ----

    #[test]
    fn checksum_chain_is_cumulative() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, key_event('+'));
        rec.step().unwrap();

        rec.push_event(2_000_000, key_event('+'));
        rec.step().unwrap();

        let trace = rec.finish();
        let frame_records: Vec<_> = trace
            .records
            .iter()
            .filter_map(|r| match r {
                TraceRecord::Frame {
                    checksum,
                    checksum_chain,
                    ..
                } => Some((*checksum, *checksum_chain)),
                _ => None,
            })
            .collect();

        assert_eq!(frame_records.len(), 3);

        // Verify chain: each chain = fnv1a64_pair(prev_chain, checksum).
        let (c0, chain0) = frame_records[0];
        assert_eq!(chain0, fnv1a64_pair(0, c0));

        let (c1, chain1) = frame_records[1];
        assert_eq!(chain1, fnv1a64_pair(chain0, c1));

        let (c2, chain2) = frame_records[2];
        assert_eq!(chain2, fnv1a64_pair(chain1, c2));

        // Final chain in summary matches last frame chain.
        assert_eq!(trace.final_checksum_chain(), Some(chain2));
    }

    // ---- Recorder program accessors ----

    #[test]
    fn recorder_exposes_program() {
        let mut rec = SessionRecorder::new(new_counter(42), 20, 1, 0);
        rec.init().unwrap();
        assert_eq!(rec.program().model().value, 42);
    }

    // ---- ReplayResult and ReplayError ----

    #[test]
    fn replay_result_ok_when_no_mismatch() {
        let r = ReplayResult {
            total_frames: 5,
            final_checksum_chain: 123,
            first_mismatch: None,
        };
        assert!(r.ok());
    }

    #[test]
    fn replay_result_not_ok_when_mismatch() {
        let r = ReplayResult {
            total_frames: 5,
            final_checksum_chain: 123,
            first_mismatch: Some(ReplayMismatch {
                frame_idx: 2,
                expected: 100,
                actual: 200,
            }),
        };
        assert!(!r.ok());
    }

    #[test]
    fn replay_error_display() {
        assert_eq!(
            ReplayError::MissingHeader.to_string(),
            "trace missing header record"
        );
        let be = ReplayError::Backend(WebBackendError::Unsupported("test"));
        assert!(be.to_string().contains("test"));
    }
}
