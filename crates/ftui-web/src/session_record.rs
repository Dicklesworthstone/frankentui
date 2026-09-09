#![forbid(unsafe_code)]

//! Deterministic session recording and replay for WASM (bd-lff4p.3.7).
//!
//! Provides [`SessionRecorder`] for recording input events, time steps, and
//! resize events during a WASM session, and [`replay`] for replaying them
//! through a fresh model to verify that frame checksums match exactly.
//!
//! # Design
//!
//! Follows the golden-trace-v2 schema defined in
//! `docs/spec/frankenterm-golden-trace-format.md`:
//!
//! - **Header**: seed, initial dimensions, capability profile.
//! - **Input**: timestamped terminal events (key, mouse, paste, etc.).
//! - **Resize**: terminal resize events.
//! - **Tick**: explicit time advancement events.
//! - **Step**: actual initialization/step boundaries, clock and execution outcomes.
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
//! recorder.push_event(0, key_event('+')).expect("key admission");
//! recorder.advance_time(16_000_000, Duration::from_millis(16));
//! recorder.step().unwrap();
//!
//! let trace = recorder.finish();
//! let result = replay(MyModel::default(), &trace).unwrap();
//! assert!(result.ok());
//! ```

use core::time::Duration;

use ftui_core::event::{
    ClipboardEvent, ClipboardSource, Event, ImeEvent, ImePhase, KeyCode, KeyEvent, KeyEventKind,
    Modifiers, MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};
use ftui_runtime::render_trace::checksum_buffer;

use crate::WebBackendError;
use crate::step_program::{StepProgram, StepResult};
#[cfg(feature = "tracing")]
use tracing::{error, info_span};

/// Schema version for session traces.
pub const SCHEMA_VERSION: &str = "golden-trace-v2";

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
    /// An observed execution boundary, including non-rendering and quit steps.
    Step {
        step_idx: u64,
        ts_ns: u64,
        init: bool,
        clock: Duration,
        result: StepResult,
    },
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

    /// Validate structural invariants for a recorded trace.
    ///
    /// This checks:
    /// - header exists and is the first record
    /// - summary exists and is the last record
    /// - frame indices are contiguous and start at zero
    /// - summary totals/chains match frame records
    /// - every execution boundary records its outcome and accepted input count
    /// - frame checkpoints follow exactly those steps that rendered
    pub fn validate(&self) -> Result<(), TraceValidationError> {
        if self.records.is_empty() {
            return Err(TraceValidationError::EmptyTrace);
        }

        let mut header_count: usize = 0;
        let mut summary: Option<(usize, u64, u64)> = None;
        let mut expected_frame_idx: u64 = 0;
        let mut frame_count: u64 = 0;
        let mut last_checksum_chain: u64 = 0;
        let mut last_ts_ns: Option<u64> = None;

        let mut validate_ts =
            |ts_ns: u64, record_index: usize| -> Result<(), TraceValidationError> {
                if let Some(previous) = last_ts_ns
                    && ts_ns < previous
                {
                    return Err(TraceValidationError::TimestampRegression {
                        previous,
                        current: ts_ns,
                        record_index,
                    });
                }
                last_ts_ns = Some(ts_ns);
                Ok(())
            };

        for (idx, record) in self.records.iter().enumerate() {
            match record {
                TraceRecord::Header { .. } => {
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
                    }
                    header_count += 1;
                }
                TraceRecord::Summary {
                    total_frames,
                    final_checksum_chain,
                } => {
                    if summary.is_some() {
                        return Err(TraceValidationError::MultipleSummaries);
                    }
                    summary = Some((idx, *total_frames, *final_checksum_chain));
                }
                TraceRecord::Frame {
                    frame_idx,
                    ts_ns,
                    checksum_chain,
                    ..
                } => {
                    validate_ts(*ts_ns, idx)?;
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
                    }
                    if *frame_idx != expected_frame_idx {
                        return Err(TraceValidationError::FrameIndexMismatch {
                            expected: expected_frame_idx,
                            actual: *frame_idx,
                        });
                    }
                    expected_frame_idx = expected_frame_idx.saturating_add(1);
                    frame_count = frame_count.saturating_add(1);
                    last_checksum_chain = *checksum_chain;
                }
                TraceRecord::Input { ts_ns, .. } => {
                    validate_ts(*ts_ns, idx)?;
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
                    }
                }
                TraceRecord::Resize { ts_ns, .. } => {
                    validate_ts(*ts_ns, idx)?;
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
                    }
                }
                TraceRecord::Tick { ts_ns } => {
                    validate_ts(*ts_ns, idx)?;
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
                    }
                }
                TraceRecord::Step { ts_ns, .. } => {
                    validate_ts(*ts_ns, idx)?;
                    if let Some((summary_index, _, _)) = summary {
                        return Err(TraceValidationError::SummaryNotLast { summary_index });
                    }
                }
            }
        }

        if header_count == 0 {
            return Err(TraceValidationError::MissingHeader);
        }
        if header_count > 1 {
            return Err(TraceValidationError::MultipleHeaders);
        }
        if !matches!(self.records.first(), Some(TraceRecord::Header { .. })) {
            return Err(TraceValidationError::HeaderNotFirst);
        }

        let Some((summary_idx, summary_frames, summary_chain)) = summary else {
            return Err(TraceValidationError::MissingSummary);
        };
        if summary_idx != self.records.len().saturating_sub(1) {
            return Err(TraceValidationError::SummaryNotLast {
                summary_index: summary_idx,
            });
        }
        if summary_frames != frame_count {
            return Err(TraceValidationError::SummaryFrameCountMismatch {
                expected: frame_count,
                actual: summary_frames,
            });
        }
        if summary_chain != last_checksum_chain {
            return Err(TraceValidationError::SummaryChecksumChainMismatch {
                expected: last_checksum_chain,
                actual: summary_chain,
            });
        }

        self.validate_steps()
    }

    fn validate_steps(&self) -> Result<(), TraceValidationError> {
        let mut next_step = 0;
        let mut frames = 0;
        let mut queued = 0_u64;
        let mut last_result: Option<StepResult> = None;
        let mut frame_pending = false;
        let mut inputs_pending = false;
        let mut checksum_chain = 0;
        for (record_index, record) in self.records.iter().enumerate() {
            let invalid = |reason| TraceValidationError::InvalidExecution {
                record_index,
                reason,
            };
            if frame_pending && !matches!(record, TraceRecord::Frame { .. }) {
                return Err(invalid("rendered step is missing its frame checkpoint"));
            }
            match record {
                TraceRecord::Step {
                    step_idx,
                    init,
                    result,
                    ..
                } => {
                    if *step_idx != next_step || *init != (next_step == 0) {
                        return Err(invalid(
                            "steps must start with init at zero and be contiguous",
                        ));
                    }
                    if *init && result.events_processed != 0 {
                        return Err(invalid("initialization cannot process queued input"));
                    }
                    if result.events_processed as u64 > queued {
                        return Err(invalid("step processed more events than were admitted"));
                    }
                    queued -= u64::from(result.events_processed);
                    if queued != u64::from(result.events_pending) {
                        return Err(invalid(
                            "step pending count does not account for accepted input",
                        ));
                    }
                    if let Some(previous) = last_result
                        && !previous.running
                        && (result.running || result.rendered || result.events_processed != 0)
                    {
                        return Err(invalid("a stopped program cannot resume or process input"));
                    }
                    if result.rendered && !result.running {
                        return Err(invalid("a stopped step cannot render"));
                    }
                    if result.frame_idx != frames + u64::from(result.rendered) {
                        return Err(invalid(
                            "step frame count does not match rendered checkpoints",
                        ));
                    }
                    frame_pending = result.rendered;
                    inputs_pending = false;
                    last_result = Some(*result);
                    next_step += 1;
                }
                TraceRecord::Frame {
                    checksum,
                    checksum_chain: supplied_chain,
                    ..
                } => {
                    if !frame_pending {
                        return Err(invalid(
                            "frame checkpoint has no rendered execution boundary",
                        ));
                    }
                    checksum_chain = fnv1a64_pair(checksum_chain, *checksum);
                    if *supplied_chain != checksum_chain {
                        return Err(invalid("frame checksum chain does not match its hashes"));
                    }
                    frame_pending = false;
                    frames += 1;
                }
                TraceRecord::Input { .. } | TraceRecord::Resize { .. } => {
                    if last_result.is_some_and(|result| !result.running) {
                        return Err(invalid(
                            "input cannot be admitted after the program stopped",
                        ));
                    }
                    queued += 1;
                    inputs_pending = true;
                }
                TraceRecord::Tick { .. } => inputs_pending = true,
                TraceRecord::Summary { .. } => {
                    if last_result.is_none() || inputs_pending {
                        return Err(invalid("trace ends without an observed execution boundary"));
                    }
                }
                TraceRecord::Header { .. } => {}
            }
        }
        Ok(())
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
    next_step_idx: u64,
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
            next_step_idx: 0,
        }
    }

    /// Record initialization and its frame checkpoint, if it renders.
    pub fn init(&mut self) -> Result<(), WebBackendError> {
        self.program.init()?;
        let result = StepResult {
            running: self.program.is_running(),
            rendered: self.program.frame_idx() > 0,
            events_processed: 0,
            events_pending: self.program.pending_events(),
            frame_idx: self.program.frame_idx(),
        };
        self.record_step(true, result);
        Ok(())
    }

    /// Record an input event at the given timestamp (nanoseconds since start).
    pub fn push_event(&mut self, ts_ns: u64, event: Event) -> Result<(), WebBackendError> {
        self.program.push_event(event.clone())?;
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Input { ts_ns, event });
        Ok(())
    }

    /// Record a resize at the given timestamp.
    pub fn resize(&mut self, ts_ns: u64, width: u16, height: u16) -> Result<(), WebBackendError> {
        self.program.resize(width, height)?;
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Resize {
            ts_ns,
            cols: width,
            rows: height,
        });
        Ok(())
    }

    /// Record a time advancement (tick) at the given timestamp.
    pub fn advance_time(&mut self, ts_ns: u64, dt: Duration) {
        self.current_ts_ns = ts_ns;
        self.records.push(TraceRecord::Tick { ts_ns });
        self.program.advance_time(dt);
    }

    /// Record an actual step, including non-rendering outcomes and pending input.
    pub fn step(&mut self) -> Result<StepResult, WebBackendError> {
        let result = self.program.step()?;
        self.record_step(false, result);
        Ok(result)
    }

    /// Finish recording and return the completed trace.
    ///
    /// Does not step or flush pending input. Validation rejects a recording
    /// whose final admissions or time advance lack an observed step boundary.
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
    ///
    /// Direct input, stepping, or queue recovery bypasses recording. Use the
    /// recorder methods for operations that must be captured in the trace.
    pub fn program_mut(&mut self) -> &mut StepProgram<M> {
        &mut self.program
    }

    fn record_step(&mut self, init: bool, result: StepResult) {
        self.records.push(TraceRecord::Step {
            step_idx: self.next_step_idx,
            ts_ns: self.current_ts_ns,
            init,
            clock: self.program.time(),
            result,
        });
        self.next_step_idx += 1;
        if result.rendered {
            self.record_frame();
        }
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
    /// Execution boundaries replayed, including initialization.
    pub total_steps: u64,
    /// Whether the model was still running at the end of the recording.
    pub running: bool,
    /// Exact accepted FIFO tail that quit prevented from being processed.
    pub unprocessed_events: Vec<Event>,
    /// Total frames replayed.
    pub total_frames: u64,
    /// Final checksum chain from replay.
    pub final_checksum_chain: u64,
    /// First frame where a checksum mismatch was detected, if any.
    pub first_mismatch: Option<ReplayMismatch>,
}

impl ReplayResult {
    /// Whether recorded execution outcomes and checksums matched.
    /// Inspect `unprocessed_events` separately: matching quit may leave a tail.
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
    /// The trace violates structural invariants.
    InvalidTrace(TraceValidationError),
    /// A backend error occurred during replay.
    Backend(WebBackendError),
    /// Replay reached a different execution outcome at a recorded boundary.
    StepMismatch {
        step_idx: u64,
        expected: StepResult,
        actual: StepResult,
    },
    /// A frame checkpoint did not have a rendered buffer to compare.
    MissingFrame { frame_idx: u64 },
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingHeader => write!(f, "trace missing header record"),
            Self::InvalidTrace(e) => write!(f, "invalid trace: {e}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
            Self::StepMismatch {
                step_idx,
                expected,
                actual,
            } => write!(
                f,
                "execution mismatch at step {step_idx}: expected {expected:?}, got {actual:?}"
            ),
            Self::MissingFrame { frame_idx } => write!(f, "missing rendered frame {frame_idx}"),
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
/// [`StepProgram`], executing only recorded initialization and step boundaries.
/// Checks exact outcomes even when a boundary did not render. Frame records
/// compare existing rendered buffers; they never trigger an extra step.
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
    trace.validate().map_err(ReplayError::InvalidTrace)?;

    let mut program = StepProgram::new(model, cols, rows);

    let mut replay_frame_idx: u64 = 0;
    let mut total_steps = 0;
    let mut checksum_chain: u64 = 0;
    let mut first_mismatch: Option<ReplayMismatch> = None;

    // Admission, actual execution boundaries and rendered checkpoints are
    // distinct. Never infer a missing step from a frame or from queued input.
    for record in &trace.records {
        match record {
            TraceRecord::Input { event, .. } => {
                program.push_event(event.clone())?;
            }
            TraceRecord::Resize { cols, rows, .. } => {
                program.resize(*cols, *rows)?;
            }
            TraceRecord::Step {
                step_idx,
                init,
                clock,
                result: expected,
                ..
            } => {
                program.set_time(*clock);
                let actual = if *init {
                    program.init()?;
                    StepResult {
                        running: program.is_running(),
                        rendered: program.frame_idx() > 0,
                        events_processed: 0,
                        events_pending: program.pending_events(),
                        frame_idx: program.frame_idx(),
                    }
                } else {
                    program.step()?
                };
                if actual != *expected {
                    return Err(ReplayError::StepMismatch {
                        step_idx: *step_idx,
                        expected: *expected,
                        actual,
                    });
                }
                total_steps += 1;
            }
            TraceRecord::Frame {
                frame_idx: expected_idx,
                checksum: expected_checksum,
                ..
            } => {
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
                } else {
                    return Err(ReplayError::MissingFrame {
                        frame_idx: *expected_idx,
                    });
                }
                replay_frame_idx += 1;
            }
            TraceRecord::Header { .. } | TraceRecord::Summary { .. } | TraceRecord::Tick { .. } => {
            }
        }
    }

    Ok(ReplayResult {
        total_steps,
        running: program.is_running(),
        unprocessed_events: program.take_pending_events(),
        total_frames: replay_frame_idx,
        final_checksum_chain: checksum_chain,
        first_mismatch,
    })
}

// ---- JSONL serialization / deserialization ----

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                use core::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn event_to_json(event: &Event) -> String {
    match event {
        Event::Key(k) => {
            let code = key_code_to_str(k.code);
            let mods = k.modifiers.bits();
            let kind = key_event_kind_to_str(k.kind);
            format!(
                r#"{{"kind":"key","code":"{}","modifiers":{},"event_kind":"{}"}}"#,
                json_escape(&code),
                mods,
                kind
            )
        }
        Event::Mouse(m) => {
            let kind = mouse_event_kind_to_str(m.kind);
            let mods = m.modifiers.bits();
            format!(
                r#"{{"kind":"mouse","mouse_kind":"{}","x":{},"y":{},"modifiers":{}}}"#,
                kind, m.x, m.y, mods
            )
        }
        Event::Resize { width, height } => {
            format!(
                r#"{{"kind":"resize","width":{},"height":{}}}"#,
                width, height
            )
        }
        Event::Paste(p) => {
            format!(
                r#"{{"kind":"paste","text":"{}","bracketed":{}}}"#,
                json_escape(&p.text),
                p.bracketed
            )
        }
        Event::Ime(ime) => {
            format!(
                r#"{{"kind":"ime","phase":"{}","text":"{}"}}"#,
                ime_phase_to_str(ime.phase),
                json_escape(&ime.text)
            )
        }
        Event::Focus(gained) => {
            format!(r#"{{"kind":"focus","gained":{}}}"#, gained)
        }
        Event::Clipboard(c) => {
            let source = clipboard_source_to_str(c.source);
            format!(
                r#"{{"kind":"clipboard","content":"{}","source":"{}"}}"#,
                json_escape(&c.content),
                source
            )
        }
        Event::Tick => r#"{"kind":"tick"}"#.to_string(),
    }
}

fn key_code_to_str(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => format!("char:{c}"),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Escape => "escape".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::F(n) => format!("f:{n}"),
        KeyCode::Null => "null".to_string(),
        KeyCode::MediaPlayPause => "media_play_pause".to_string(),
        KeyCode::MediaStop => "media_stop".to_string(),
        KeyCode::MediaNextTrack => "media_next".to_string(),
        KeyCode::MediaPrevTrack => "media_prev".to_string(),
    }
}

fn key_event_kind_to_str(kind: KeyEventKind) -> &'static str {
    match kind {
        KeyEventKind::Press => "press",
        KeyEventKind::Repeat => "repeat",
        KeyEventKind::Release => "release",
    }
}

fn mouse_event_kind_to_str(kind: MouseEventKind) -> &'static str {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => "down_left",
        MouseEventKind::Down(MouseButton::Right) => "down_right",
        MouseEventKind::Down(MouseButton::Middle) => "down_middle",
        MouseEventKind::Up(MouseButton::Left) => "up_left",
        MouseEventKind::Up(MouseButton::Right) => "up_right",
        MouseEventKind::Up(MouseButton::Middle) => "up_middle",
        MouseEventKind::Drag(MouseButton::Left) => "drag_left",
        MouseEventKind::Drag(MouseButton::Right) => "drag_right",
        MouseEventKind::Drag(MouseButton::Middle) => "drag_middle",
        MouseEventKind::Moved => "moved",
        MouseEventKind::ScrollUp => "scroll_up",
        MouseEventKind::ScrollDown => "scroll_down",
        MouseEventKind::ScrollLeft => "scroll_left",
        MouseEventKind::ScrollRight => "scroll_right",
    }
}

fn clipboard_source_to_str(source: ClipboardSource) -> &'static str {
    match source {
        ClipboardSource::Osc52 => "osc52",
        ClipboardSource::Unknown => "unknown",
    }
}

fn ime_phase_to_str(phase: ImePhase) -> &'static str {
    match phase {
        ImePhase::Start => "start",
        ImePhase::Update => "update",
        ImePhase::Commit => "commit",
        ImePhase::Cancel => "cancel",
    }
}

impl TraceRecord {
    /// Serialize this record as a golden-trace-v2 JSONL line.
    pub fn to_jsonl(&self) -> String {
        match self {
            TraceRecord::Header {
                seed,
                cols,
                rows,
                profile,
            } => format!(
                r#"{{"schema_version":"{}","event":"trace_header","seed":{},"cols":{},"rows":{},"env":{{"target":"web"}},"profile":"{}"}}"#,
                SCHEMA_VERSION,
                seed,
                cols,
                rows,
                json_escape(profile)
            ),
            TraceRecord::Input { ts_ns, event } => format!(
                r#"{{"schema_version":"{}","event":"input","ts_ns":{},"data":{}}}"#,
                SCHEMA_VERSION,
                ts_ns,
                event_to_json(event)
            ),
            TraceRecord::Resize { ts_ns, cols, rows } => format!(
                r#"{{"schema_version":"{}","event":"resize","ts_ns":{},"cols":{},"rows":{}}}"#,
                SCHEMA_VERSION, ts_ns, cols, rows
            ),
            TraceRecord::Tick { ts_ns } => format!(
                r#"{{"schema_version":"{}","event":"tick","ts_ns":{}}}"#,
                SCHEMA_VERSION, ts_ns
            ),
            TraceRecord::Step {
                step_idx,
                ts_ns,
                init,
                clock,
                result,
            } => format!(
                r#"{{"schema_version":"{}","event":"step","step_idx":{},"ts_ns":{},"init":{},"clock_secs":{},"clock_subsec_nanos":{},"running":{},"rendered":{},"events_processed":{},"events_pending":{},"frame_idx":{}}}"#,
                SCHEMA_VERSION,
                step_idx,
                ts_ns,
                init,
                clock.as_secs(),
                clock.subsec_nanos(),
                result.running,
                result.rendered,
                result.events_processed,
                result.events_pending,
                result.frame_idx
            ),
            TraceRecord::Frame {
                frame_idx,
                ts_ns,
                checksum,
                checksum_chain,
            } => format!(
                r#"{{"schema_version":"{}","event":"frame","frame_idx":{},"ts_ns":{},"hash_algo":"fnv1a64","frame_hash":"{:016x}","checksum_chain":"{:016x}"}}"#,
                SCHEMA_VERSION, frame_idx, ts_ns, checksum, checksum_chain
            ),
            TraceRecord::Summary {
                total_frames,
                final_checksum_chain,
            } => format!(
                r#"{{"schema_version":"{}","event":"trace_summary","total_frames":{},"final_checksum_chain":"{:016x}"}}"#,
                SCHEMA_VERSION, total_frames, final_checksum_chain
            ),
        }
    }
}

impl SessionTrace {
    /// Serialize the entire trace as a golden-trace-v2 JSONL string.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for record in &self.records {
            out.push_str(&record.to_jsonl());
            out.push('\n');
        }
        out
    }

    /// Parse a golden-trace-v2 JSONL string into a `SessionTrace`.
    ///
    /// Returns a parse error with the line number on failure.
    pub fn from_jsonl(input: &str) -> Result<Self, TraceParseError> {
        let mut records = Vec::new();
        for (line_num, line) in input.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let record = parse_trace_line(line, line_num + 1)?;
            records.push(record);
        }
        Ok(SessionTrace { records })
    }

    /// Parse and validate a golden-trace-v2 JSONL payload.
    pub fn from_jsonl_validated(input: &str) -> Result<Self, TraceLoadError> {
        let trace = Self::from_jsonl(input)?;
        trace.validate()?;
        Ok(trace)
    }
}

/// Error parsing a JSONL trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceParseError {
    pub line: usize,
    pub message: String,
}

impl core::fmt::Display for TraceParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TraceParseError {}

/// Typed validation failures for `SessionTrace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceValidationError {
    InvalidExecution {
        record_index: usize,
        reason: &'static str,
    },
    EmptyTrace,
    MissingHeader,
    HeaderNotFirst,
    MultipleHeaders,
    MissingSummary,
    MultipleSummaries,
    SummaryNotLast {
        summary_index: usize,
    },
    TimestampRegression {
        previous: u64,
        current: u64,
        record_index: usize,
    },
    FrameIndexMismatch {
        expected: u64,
        actual: u64,
    },
    SummaryFrameCountMismatch {
        expected: u64,
        actual: u64,
    },
    SummaryChecksumChainMismatch {
        expected: u64,
        actual: u64,
    },
}

impl core::fmt::Display for TraceValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidExecution {
                record_index,
                reason,
            } => {
                write!(f, "invalid execution at record {record_index}: {reason}")
            }
            Self::EmptyTrace => write!(f, "trace is empty"),
            Self::MissingHeader => write!(f, "trace is missing header"),
            Self::HeaderNotFirst => write!(f, "trace header is not the first record"),
            Self::MultipleHeaders => write!(f, "trace contains multiple headers"),
            Self::MissingSummary => write!(f, "trace is missing summary"),
            Self::MultipleSummaries => write!(f, "trace contains multiple summaries"),
            Self::SummaryNotLast { summary_index } => write!(
                f,
                "trace summary at index {} is not the final record",
                summary_index
            ),
            Self::TimestampRegression {
                previous,
                current,
                record_index,
            } => write!(
                f,
                "timestamp regression at record {}: current ts_ns={} is less than previous ts_ns={}",
                record_index, current, previous
            ),
            Self::FrameIndexMismatch { expected, actual } => {
                write!(
                    f,
                    "frame index mismatch: expected {}, got {}",
                    expected, actual
                )
            }
            Self::SummaryFrameCountMismatch { expected, actual } => write!(
                f,
                "summary frame-count mismatch: expected {}, got {}",
                expected, actual
            ),
            Self::SummaryChecksumChainMismatch { expected, actual } => write!(
                f,
                "summary checksum-chain mismatch: expected {:016x}, got {:016x}",
                expected, actual
            ),
        }
    }
}

impl std::error::Error for TraceValidationError {}

/// Combined load error for parse + validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceLoadError {
    Parse(TraceParseError),
    Validation(TraceValidationError),
}

impl core::fmt::Display for TraceLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{e}"),
            Self::Validation(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TraceLoadError {}

impl From<TraceParseError> for TraceLoadError {
    fn from(value: TraceParseError) -> Self {
        Self::Parse(value)
    }
}

impl From<TraceValidationError> for TraceLoadError {
    fn from(value: TraceValidationError) -> Self {
        Self::Validation(value)
    }
}

// ---- Minimal JSON field extraction (no serde dependency) ----

fn json_string_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return Some(i + 1);
        }
        if bytes[i] < 0x20 {
            return None;
        }
        i += 1;
    }
    None
}

fn json_value_end(input: &str) -> Option<usize> {
    match input.as_bytes().first()? {
        b'"' => json_string_end(input),
        b'{' | b'[' => {
            let mut closing = Vec::new();
            let mut index = 0;
            while index < input.len() {
                match input.as_bytes()[index] {
                    b'"' => {
                        index += json_string_end(&input[index..])?;
                        continue;
                    }
                    b'{' => closing.push(b'}'),
                    b'[' => closing.push(b']'),
                    byte @ (b'}' | b']') => {
                        if closing.pop()? != byte {
                            return None;
                        }
                        if closing.is_empty() {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => Some(input.find([',', '}', ']']).unwrap_or(input.len())),
    }
}

// Only inspect complete top-level values. A nested field or quoted payload
// must never impersonate execution metadata; duplicate requested keys fail.
fn extract_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = json.trim().strip_prefix('{')?.trim_start();
    let mut found = None;
    if rest == "}" {
        return None;
    }
    loop {
        let key_end = json_string_end(rest)?;
        let field_key = &rest[1..key_end - 1];
        rest = rest[key_end..].trim_start().strip_prefix(':')?.trim_start();
        let value_end = json_value_end(rest)?;
        let value = rest[..value_end].trim();
        if value.is_empty() {
            return None;
        }
        if field_key == key {
            if found.is_some() {
                return None;
            }
            found = Some(value);
        }
        rest = rest[value_end..].trim_start();
        if rest == "}" {
            return found;
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn extract_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    extract_value(json, key)?
        .strip_prefix('"')?
        .strip_suffix('"')
}

fn extract_u64(json: &str, key: &str) -> Option<u64> {
    let value = extract_value(json, key)?;
    if !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn extract_i64(json: &str, key: &str) -> Option<i64> {
    let value = extract_value(json, key)?;
    let digits = value.strip_prefix('-').unwrap_or(value);
    if !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn extract_u16(json: &str, key: &str) -> Option<u16> {
    extract_u64(json, key).and_then(|v| u16::try_from(v).ok())
}

fn extract_bool(json: &str, key: &str) -> Option<bool> {
    match extract_value(json, key)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn extract_hex_u64(json: &str, key: &str) -> Option<u64> {
    let s = extract_str(json, key)?;
    u64::from_str_radix(s, 16).ok()
}

fn extract_object<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let value = extract_value(json, key)?;
    (value.starts_with('{') && value.ends_with('}')).then_some(value)
}

fn json_unescape(input: &str) -> Result<String, String> {
    fn hex_unit(chars: &mut core::str::Chars<'_>) -> Result<u32, String> {
        let mut unit = 0;
        for _ in 0..4 {
            let digit = chars
                .next()
                .and_then(|ch| ch.to_digit(16))
                .ok_or("invalid JSON Unicode escape")?;
            unit = unit * 16 + digit;
        }
        Ok(unit)
    }

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let mut cp = hex_unit(&mut chars)?;
                    if (0xd800..=0xdbff).contains(&cp) {
                        if chars.next() != Some('\\') || chars.next() != Some('u') {
                            return Err("missing JSON low surrogate".to_string());
                        }
                        let low = hex_unit(&mut chars)?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err("invalid JSON low surrogate".to_string());
                        }
                        cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                    }
                    out.push(char::from_u32(cp).ok_or("invalid JSON Unicode scalar")?);
                }
                Some(_) => return Err("invalid JSON escape".to_string()),
                None => return Err("incomplete JSON escape".to_string()),
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

fn check_trace_schema_compat(schema_version: &str, line_num: usize) -> Result<(), TraceParseError> {
    let incompatible = schema_version != SCHEMA_VERSION;

    #[cfg(feature = "tracing")]
    {
        let span = info_span!(
            "trace.compat_check",
            reader_schema_version = SCHEMA_VERSION,
            writer_schema_version = schema_version,
            line = line_num,
            compatible = !incompatible,
        );
        let _guard = span.enter();

        if incompatible {
            error!(
                reader_schema_version = SCHEMA_VERSION,
                writer_schema_version = schema_version,
                line = line_num,
                "trace schema version incompatible"
            );
        }
    }

    if incompatible {
        return Err(TraceParseError {
            line: line_num,
            message: format!(
                "unsupported schema_version: {schema_version} (reader={SCHEMA_VERSION}, migration required)"
            ),
        });
    }
    Ok(())
}

fn parse_trace_line(line: &str, line_num: usize) -> Result<TraceRecord, TraceParseError> {
    let err = |msg: &str| TraceParseError {
        line: line_num,
        message: msg.to_string(),
    };

    let schema_version = extract_str(line, "schema_version")
        .ok_or_else(|| err("missing \"schema_version\" field"))?;
    check_trace_schema_compat(schema_version, line_num)?;

    let event = extract_str(line, "event").ok_or_else(|| err("missing \"event\" field"))?;

    match event {
        "trace_header" => {
            let seed = extract_u64(line, "seed").unwrap_or(0);
            let cols = extract_u16(line, "cols").ok_or_else(|| err("missing cols"))?;
            let rows = extract_u16(line, "rows").ok_or_else(|| err("missing rows"))?;
            let profile = extract_str(line, "profile")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "modern".to_string());
            Ok(TraceRecord::Header {
                seed,
                cols,
                rows,
                profile,
            })
        }
        "input" => {
            let ts_ns = extract_u64(line, "ts_ns").ok_or_else(|| err("missing ts_ns"))?;
            let data = extract_object(line, "data").ok_or_else(|| err("missing data object"))?;
            let event = parse_event_json(data).map_err(|e| err(&e))?;
            Ok(TraceRecord::Input { ts_ns, event })
        }
        "resize" => {
            let ts_ns = extract_u64(line, "ts_ns").ok_or_else(|| err("missing ts_ns"))?;
            let cols = extract_u16(line, "cols").ok_or_else(|| err("missing cols"))?;
            let rows = extract_u16(line, "rows").ok_or_else(|| err("missing rows"))?;
            Ok(TraceRecord::Resize { ts_ns, cols, rows })
        }
        "tick" => {
            let ts_ns = extract_u64(line, "ts_ns").ok_or_else(|| err("missing ts_ns"))?;
            Ok(TraceRecord::Tick { ts_ns })
        }
        "step" => {
            let required_u64 = |key| {
                extract_u64(line, key).ok_or_else(|| err(&format!("missing or invalid {key}")))
            };
            let required_u32 = |key| {
                required_u64(key).and_then(|value| {
                    u32::try_from(value).map_err(|_| err(&format!("{key} out of range")))
                })
            };
            let required_bool = |key| {
                extract_bool(line, key).ok_or_else(|| err(&format!("missing or invalid {key}")))
            };
            let nanos = required_u32("clock_subsec_nanos")?;
            if nanos >= 1_000_000_000 {
                return Err(err("clock_subsec_nanos must be less than 1000000000"));
            }
            Ok(TraceRecord::Step {
                step_idx: required_u64("step_idx")?,
                ts_ns: required_u64("ts_ns")?,
                init: required_bool("init")?,
                clock: Duration::new(required_u64("clock_secs")?, nanos),
                result: StepResult {
                    running: required_bool("running")?,
                    rendered: required_bool("rendered")?,
                    events_processed: required_u32("events_processed")?,
                    events_pending: required_u32("events_pending")?,
                    frame_idx: required_u64("frame_idx")?,
                },
            })
        }
        "frame" => {
            let frame_idx =
                extract_u64(line, "frame_idx").ok_or_else(|| err("missing frame_idx"))?;
            let ts_ns = extract_u64(line, "ts_ns").ok_or_else(|| err("missing ts_ns"))?;
            let checksum =
                extract_hex_u64(line, "frame_hash").ok_or_else(|| err("missing frame_hash"))?;
            let checksum_chain = extract_hex_u64(line, "checksum_chain")
                .ok_or_else(|| err("missing checksum_chain"))?;
            Ok(TraceRecord::Frame {
                frame_idx,
                ts_ns,
                checksum,
                checksum_chain,
            })
        }
        "trace_summary" => {
            let total_frames =
                extract_u64(line, "total_frames").ok_or_else(|| err("missing total_frames"))?;
            let final_checksum_chain = extract_hex_u64(line, "final_checksum_chain")
                .ok_or_else(|| err("missing final_checksum_chain"))?;
            Ok(TraceRecord::Summary {
                total_frames,
                final_checksum_chain,
            })
        }
        other => Err(err(&format!("unknown event type: {other}"))),
    }
}

fn parse_event_json(data: &str) -> Result<Event, String> {
    let kind = extract_str(data, "kind").ok_or("missing event kind")?;
    match kind {
        "key" => {
            let code_str = extract_str(data, "code").ok_or("missing key code")?;
            let code = parse_key_code(&json_unescape(code_str)?)?;
            let mods_bits = extract_u64(data, "modifiers")
                .or(extract_u64(data, "mods"))
                .unwrap_or(0) as u8;
            let modifiers = Modifiers::from_bits(mods_bits).unwrap_or_else(Modifiers::empty);
            let event_kind = if let Some(event_kind_str) = extract_str(data, "event_kind") {
                match event_kind_str {
                    "press" => KeyEventKind::Press,
                    "repeat" => KeyEventKind::Repeat,
                    "release" => KeyEventKind::Release,
                    _ => KeyEventKind::Press,
                }
            } else {
                let phase = extract_str(data, "phase").unwrap_or("down");
                let repeat = extract_bool(data, "repeat").unwrap_or(false);
                parse_key_event_kind(phase, repeat)
            };
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind: event_kind,
            }))
        }
        "mouse" => {
            let mouse_kind = if let Some(mouse_kind_str) = extract_str(data, "mouse_kind") {
                parse_mouse_event_kind(mouse_kind_str)?
            } else {
                let phase = extract_str(data, "phase").ok_or("missing phase for mouse event")?;
                let button = extract_u64(data, "button")
                    .map(|raw| {
                        u8::try_from(raw).map_err(|_| "mouse button out of range".to_string())
                    })
                    .transpose()?;
                parse_mouse_phase_and_button(phase, button)?
            };
            let x = extract_u16(data, "x").unwrap_or(0);
            let y = extract_u16(data, "y").unwrap_or(0);
            let mods_bits = extract_u64(data, "modifiers")
                .or(extract_u64(data, "mods"))
                .unwrap_or(0) as u8;
            let modifiers = Modifiers::from_bits(mods_bits).unwrap_or_else(Modifiers::empty);
            Ok(Event::Mouse(MouseEvent {
                kind: mouse_kind,
                x,
                y,
                modifiers,
            }))
        }
        "wheel" => {
            let x = extract_u16(data, "x").unwrap_or(0);
            let y = extract_u16(data, "y").unwrap_or(0);
            let dx = extract_i64(data, "dx")
                .and_then(|value| i16::try_from(value).ok())
                .unwrap_or(0);
            let dy = extract_i64(data, "dy")
                .and_then(|value| i16::try_from(value).ok())
                .unwrap_or(0);
            let kind = if dy < 0 {
                MouseEventKind::ScrollUp
            } else if dy > 0 {
                MouseEventKind::ScrollDown
            } else if dx < 0 {
                MouseEventKind::ScrollLeft
            } else if dx > 0 {
                MouseEventKind::ScrollRight
            } else {
                return Err("wheel event must include non-zero dx or dy".to_string());
            };
            let mods_bits = extract_u64(data, "modifiers")
                .or(extract_u64(data, "mods"))
                .unwrap_or(0) as u8;
            let modifiers = Modifiers::from_bits(mods_bits).unwrap_or_else(Modifiers::empty);
            Ok(Event::Mouse(MouseEvent {
                kind,
                x,
                y,
                modifiers,
            }))
        }
        "resize" => {
            let width = extract_u16(data, "width").ok_or("missing width")?;
            let height = extract_u16(data, "height").ok_or("missing height")?;
            Ok(Event::Resize { width, height })
        }
        "paste" => {
            let text = extract_str(data, "text")
                .or(extract_str(data, "data"))
                .map(json_unescape)
                .transpose()?
                .unwrap_or_default();
            let bracketed = extract_bool(data, "bracketed").unwrap_or(true);
            Ok(Event::Paste(PasteEvent::new(text, bracketed)))
        }
        "ime" | "composition" => {
            let phase_raw = extract_str(data, "phase").unwrap_or("update");
            let phase = parse_ime_phase(phase_raw)?;
            let text = extract_str(data, "text")
                .or(extract_str(data, "data"))
                .map(json_unescape)
                .transpose()?
                .unwrap_or_default();
            let ime = match phase {
                ImePhase::Start => ImeEvent::start(),
                ImePhase::Update => ImeEvent::update(text),
                ImePhase::Commit => ImeEvent::commit(text),
                ImePhase::Cancel => ImeEvent::cancel(),
            };
            Ok(Event::Ime(ime))
        }
        "focus" => {
            let gained = extract_bool(data, "gained")
                .or(extract_bool(data, "focused"))
                .unwrap_or(true);
            Ok(Event::Focus(gained))
        }
        "clipboard" => {
            let content = extract_str(data, "content")
                .map(json_unescape)
                .transpose()?
                .unwrap_or_default();
            let source_str = extract_str(data, "source").unwrap_or("unknown");
            let source = match source_str {
                "osc52" => ClipboardSource::Osc52,
                _ => ClipboardSource::Unknown,
            };
            Ok(Event::Clipboard(ClipboardEvent::new(content, source)))
        }
        "tick" => Ok(Event::Tick),
        other => Err(format!("unknown event kind: {other}")),
    }
}

fn parse_ime_phase(phase: &str) -> Result<ImePhase, String> {
    match phase {
        "start" => Ok(ImePhase::Start),
        "update" => Ok(ImePhase::Update),
        "end" | "commit" => Ok(ImePhase::Commit),
        "cancel" => Ok(ImePhase::Cancel),
        other => Err(format!("unknown ime/composition phase: {other}")),
    }
}

fn parse_key_code(s: &str) -> Result<KeyCode, String> {
    if let Some(rest) = s.strip_prefix("char:") {
        let ch = rest.chars().next().ok_or("empty char code")?;
        return Ok(KeyCode::Char(ch));
    }
    if let Some(rest) = s.strip_prefix("f:") {
        let n: u8 = rest.parse().map_err(|_| "invalid F-key number")?;
        return Ok(KeyCode::F(n));
    }
    if let Some(n) = parse_function_key_token(s) {
        return Ok(KeyCode::F(n));
    }

    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
    {
        return Ok(KeyCode::Char(ch));
    }

    let normalized = s.to_ascii_lowercase();
    match normalized.as_str() {
        "enter" | "return" => Ok(KeyCode::Enter),
        "escape" | "esc" => Ok(KeyCode::Escape),
        "backspace" => Ok(KeyCode::Backspace),
        "tab" => Ok(KeyCode::Tab),
        "backtab" => Ok(KeyCode::BackTab),
        "delete" => Ok(KeyCode::Delete),
        "insert" => Ok(KeyCode::Insert),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "pageup" => Ok(KeyCode::PageUp),
        "pagedown" => Ok(KeyCode::PageDown),
        "up" | "arrowup" => Ok(KeyCode::Up),
        "down" | "arrowdown" => Ok(KeyCode::Down),
        "left" | "arrowleft" => Ok(KeyCode::Left),
        "right" | "arrowright" => Ok(KeyCode::Right),
        "null" | "unidentified" => Ok(KeyCode::Null),
        "media_play_pause" => Ok(KeyCode::MediaPlayPause),
        "media_stop" => Ok(KeyCode::MediaStop),
        "media_next" => Ok(KeyCode::MediaNextTrack),
        "media_prev" => Ok(KeyCode::MediaPrevTrack),
        other => Err(format!("unknown key code: {other}")),
    }
}

fn parse_function_key_token(s: &str) -> Option<u8> {
    let rest = s.strip_prefix('F').or_else(|| s.strip_prefix('f'))?;
    if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn parse_key_event_kind(phase: &str, repeat: bool) -> KeyEventKind {
    if phase.eq_ignore_ascii_case("up") || phase.eq_ignore_ascii_case("release") {
        KeyEventKind::Release
    } else if repeat {
        KeyEventKind::Repeat
    } else {
        KeyEventKind::Press
    }
}

fn parse_mouse_event_kind(s: &str) -> Result<MouseEventKind, String> {
    match s {
        "down_left" => Ok(MouseEventKind::Down(MouseButton::Left)),
        "down_right" => Ok(MouseEventKind::Down(MouseButton::Right)),
        "down_middle" => Ok(MouseEventKind::Down(MouseButton::Middle)),
        "up_left" => Ok(MouseEventKind::Up(MouseButton::Left)),
        "up_right" => Ok(MouseEventKind::Up(MouseButton::Right)),
        "up_middle" => Ok(MouseEventKind::Up(MouseButton::Middle)),
        "drag_left" => Ok(MouseEventKind::Drag(MouseButton::Left)),
        "drag_right" => Ok(MouseEventKind::Drag(MouseButton::Right)),
        "drag_middle" => Ok(MouseEventKind::Drag(MouseButton::Middle)),
        "moved" => Ok(MouseEventKind::Moved),
        "scroll_up" => Ok(MouseEventKind::ScrollUp),
        "scroll_down" => Ok(MouseEventKind::ScrollDown),
        "scroll_left" => Ok(MouseEventKind::ScrollLeft),
        "scroll_right" => Ok(MouseEventKind::ScrollRight),
        other => Err(format!("unknown mouse event kind: {other}")),
    }
}

fn parse_mouse_phase_and_button(phase: &str, button: Option<u8>) -> Result<MouseEventKind, String> {
    match phase {
        "down" => Ok(MouseEventKind::Down(parse_mouse_button(
            button.ok_or("mouse down requires button")?,
        )?)),
        "up" => Ok(MouseEventKind::Up(parse_mouse_button(
            button.ok_or("mouse up requires button")?,
        )?)),
        "drag" => Ok(MouseEventKind::Drag(parse_mouse_button(
            button.ok_or("mouse drag requires button")?,
        )?)),
        "move" => Ok(MouseEventKind::Moved),
        other => Err(format!("unknown mouse phase: {other}")),
    }
}

fn parse_mouse_button(raw: u8) -> Result<MouseButton, String> {
    match raw {
        0 => Ok(MouseButton::Left),
        1 => Ok(MouseButton::Middle),
        2 => Ok(MouseButton::Right),
        other => Err(format!("unsupported mouse button: {other}")),
    }
}

// ---- Golden Gate API ----

/// Validate a trace against a fresh model, returning a detailed report.
///
/// This is the primary entry point for CI checksum gates. It replays the
/// trace and produces a [`GateReport`] with pass/fail status and actionable
/// diff information on any mismatch.
pub fn gate_trace<M: ftui_runtime::program::Model>(
    model: M,
    trace: &SessionTrace,
) -> Result<GateReport, ReplayError> {
    let result = replay(model, trace)?;

    let frame_checksums: Vec<(u64, u64)> = trace
        .records
        .iter()
        .filter_map(|r| match r {
            TraceRecord::Frame {
                frame_idx,
                checksum,
                ..
            } => Some((*frame_idx, *checksum)),
            _ => None,
        })
        .collect();

    let diff = result.first_mismatch.as_ref().map(|m| {
        // Find the event context: count Input/Resize/Tick records before the failing frame.
        let mut event_idx: u64 = 0;
        let mut last_event_desc = String::new();
        let mut frame_count: u64 = 0;
        for record in &trace.records {
            match record {
                TraceRecord::Frame { .. } => {
                    if frame_count == m.frame_idx {
                        break;
                    }
                    frame_count += 1;
                }
                TraceRecord::Input { event, .. } => {
                    last_event_desc = format!("{event:?}");
                    event_idx += 1;
                }
                TraceRecord::Resize { cols, rows, .. } => {
                    last_event_desc = format!("Resize({cols}x{rows})");
                    event_idx += 1;
                }
                TraceRecord::Tick { ts_ns } => {
                    last_event_desc = format!("Tick(ts_ns={ts_ns})");
                    event_idx += 1;
                }
                _ => {}
            }
        }

        GateDiff {
            frame_idx: m.frame_idx,
            event_idx,
            last_event: last_event_desc,
            expected_checksum: m.expected,
            actual_checksum: m.actual,
        }
    });

    Ok(GateReport {
        passed: result.ok(),
        total_steps: result.total_steps,
        running: result.running,
        unprocessed_events: result.unprocessed_events,
        total_frames: result.total_frames,
        expected_frames: frame_checksums.len() as u64,
        final_checksum_chain: result.final_checksum_chain,
        diff,
    })
}

/// Report from a golden trace gate validation.
#[derive(Debug, Clone)]
pub struct GateReport {
    /// Whether all execution outcomes and frame checksums matched.
    pub passed: bool,
    /// Number of execution boundaries replayed, including initialization.
    pub total_steps: u64,
    /// Whether the replayed program is still running.
    pub running: bool,
    /// Accepted input left unprocessed after quit, preserved in FIFO order.
    pub unprocessed_events: Vec<Event>,
    /// Number of frames replayed.
    pub total_frames: u64,
    /// Number of frame checkpoints in the trace.
    pub expected_frames: u64,
    /// Final checksum chain from replay.
    pub final_checksum_chain: u64,
    /// Detailed diff information if there was a mismatch.
    pub diff: Option<GateDiff>,
}

impl GateReport {
    /// Format the report as a human-readable string.
    pub fn format(&self) -> String {
        if self.passed {
            format!(
                "PASS: {}/{} frames verified, final_chain={:016x}, steps={}, running={}, unprocessed_events={}",
                self.total_frames,
                self.expected_frames,
                self.final_checksum_chain,
                self.total_steps,
                self.running,
                self.unprocessed_events.len()
            )
        } else if let Some(d) = &self.diff {
            format!(
                "FAIL at frame {} (after event #{}: {}): expected {:016x}, got {:016x}",
                d.frame_idx, d.event_idx, d.last_event, d.expected_checksum, d.actual_checksum
            )
        } else {
            format!(
                "FAIL: {}/{} frames, unknown mismatch",
                self.total_frames, self.expected_frames
            )
        }
    }
}

/// Detailed diff information for a checksum mismatch.
#[derive(Debug, Clone)]
pub struct GateDiff {
    /// Frame index where the mismatch occurred.
    pub frame_idx: u64,
    /// Number of input events processed before the failing frame.
    pub event_idx: u64,
    /// Description of the last event before the failing frame.
    pub last_event: String,
    /// Expected checksum from the trace.
    pub expected_checksum: u64,
    /// Actual checksum from replay.
    pub actual_checksum: u64,
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
    #[cfg(feature = "tracing")]
    use std::sync::{Arc, Mutex};
    #[cfg(feature = "tracing")]
    use tracing::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing::field::{Field, Visit};
    #[cfg(feature = "tracing")]
    use tracing_subscriber::Layer;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::filter::LevelFilter;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, SubscriberExt};
    #[cfg(feature = "tracing")]
    use tracing_subscriber::registry::LookupSpan;

    #[cfg(feature = "tracing")]
    #[derive(Default, Clone)]
    struct TraceCaptureLayer {
        spans: Arc<Mutex<Vec<String>>>,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[cfg(feature = "tracing")]
    #[derive(Default)]
    struct EventMessageVisitor {
        message: Option<String>,
    }

    #[cfg(feature = "tracing")]
    impl Visit for EventMessageVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.message = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}"));
            }
        }
    }

    #[cfg(feature = "tracing")]
    impl<S> Layer<S> for TraceCaptureLayer
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            self.spans
                .lock()
                .expect("span capture lock")
                .push(attrs.metadata().name().to_string());
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = EventMessageVisitor::default();
            event.record(&mut visitor);
            let message = visitor.message.unwrap_or_default();
            self.events
                .lock()
                .expect("event capture lock")
                .push(format!("{}:{}", event.metadata().level(), message));
        }
    }

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

    fn parse_single_input_event(data_json: &str) -> Event {
        let line = format!(
            r#"{{"schema_version":"{}","event":"input","ts_ns":0,"data":{}}}"#,
            SCHEMA_VERSION, data_json
        );
        let trace = SessionTrace::from_jsonl(&line).expect("input JSON should parse");
        trace
            .records
            .into_iter()
            .next()
            .and_then(|record| match record {
                TraceRecord::Input { event, .. } => Some(event),
                _ => None,
            })
            .expect("expected single input record")
    }

    fn new_counter(value: i32) -> Counter {
        Counter { value }
    }

    // Observe actual updates even when quit prevents any new frame.
    struct ObservedModel {
        seen: std::rc::Rc<std::cell::RefCell<Vec<Event>>>,
        init_count: std::rc::Rc<std::cell::Cell<u32>>,
        quit_on_init: bool,
        honor_quit: bool,
    }

    impl Model for ObservedModel {
        type Message = Event;

        fn init(&mut self) -> Cmd<Event> {
            self.init_count.set(self.init_count.get() + 1);
            if self.quit_on_init {
                Cmd::quit()
            } else {
                Cmd::tick(Duration::from_millis(10))
            }
        }

        fn update(&mut self, event: Event) -> Cmd<Event> {
            let quit = event == key_event('q') && self.honor_quit;
            self.seen.borrow_mut().push(event);
            if quit { Cmd::quit() } else { Cmd::none() }
        }

        fn view(&self, frame: &mut Frame) {
            frame.buffer.set_raw(0, 0, Cell::from_char('X'));
        }
    }

    fn observed_model(quit_on_init: bool, honor_quit: bool) -> ObservedModel {
        ObservedModel {
            seen: Default::default(),
            init_count: Default::default(),
            quit_on_init,
            honor_quit,
        }
    }

    #[test]
    fn non_rendering_quit_replays_actual_updates_and_exact_accepted_tail() {
        let model = observed_model(false, true);
        let recorded_seen = model.seen.clone();
        let mut recorder = SessionRecorder::new(model, 20, 2, 7);
        recorder.init().unwrap();
        let tail = vec![
            Event::Paste(PasteEvent::bracketed("尾巴 🦀")),
            Event::Resize {
                width: 33,
                height: 4,
            },
        ];
        for event in [key_event('+'), key_event('q')]
            .into_iter()
            .chain(tail.clone())
        {
            recorder.push_event(9, event).unwrap();
        }
        let outcome = recorder.step().unwrap();
        assert_eq!(outcome.events_processed, 2);
        assert_eq!(outcome.events_pending, 2);
        assert!(!outcome.running && !outcome.rendered);
        assert_eq!(
            *recorded_seen.borrow(),
            vec![key_event('+'), key_event('q')]
        );
        let trace = SessionTrace::from_jsonl_validated(&recorder.finish().to_jsonl()).unwrap();
        let model = observed_model(false, true);
        let replayed_seen = model.seen.clone();
        let init_count = model.init_count.clone();
        let result = replay(model, &trace).unwrap();
        assert!(result.ok());
        assert_eq!(result.total_steps, 2);
        assert_eq!(result.total_frames, 1);
        assert!(!result.running);
        assert_eq!(result.unprocessed_events, tail);
        assert_eq!(*replayed_seen.borrow(), *recorded_seen.borrow());
        assert_eq!(init_count.get(), 1);
        let gate = gate_trace(observed_model(false, true), &trace).unwrap();
        assert!(gate.passed);
        assert_eq!(gate.total_steps, 2);
        assert!(!gate.running);
        assert_eq!(gate.unprocessed_events, tail);
        assert!(gate.format().contains("unprocessed_events=2"));
        assert!(!gate.format().contains("尾巴"));

        // Same initial frame, different quit behavior: checksum-only replay
        // previously accepted this without ever executing the quit step.
        assert!(matches!(
            replay(observed_model(false, false), &trace),
            Err(ReplayError::StepMismatch { step_idx: 1, .. })
        ));
    }

    #[test]
    fn initialization_quit_has_no_frame_and_recovers_preinit_input() {
        let mut recorder = SessionRecorder::new(observed_model(true, true), 20, 2, 7);
        recorder.push_event(1, key_event('+')).unwrap();
        recorder.resize(2, 40, 4).unwrap();
        recorder.init().unwrap();
        assert_eq!(recorder.program().size(), (20, 2));
        assert!(recorder.program().outputs().last_buffer.is_none());
        let stopped = recorder.step().unwrap();
        assert_eq!(stopped.events_pending, 2);
        assert_eq!(stopped.events_processed, 0);
        let trace = SessionTrace::from_jsonl_validated(&recorder.finish().to_jsonl()).unwrap();
        let model = observed_model(true, true);
        let seen = model.seen.clone();
        let init_count = model.init_count.clone();
        let result = replay(model, &trace).unwrap();
        assert!(result.ok());
        assert!(!result.running);
        assert_eq!(result.total_frames, 0);
        assert_eq!(result.total_steps, 2);
        assert_eq!(init_count.get(), 1);
        assert!(seen.borrow().is_empty());
        assert_eq!(
            result.unprocessed_events,
            vec![
                key_event('+'),
                Event::Resize {
                    width: 40,
                    height: 4
                }
            ]
        );
    }

    #[test]
    fn idle_steps_and_clock_deltas_replay_independently_of_metadata_timestamps() {
        let model = observed_model(false, true);
        let seen = model.seen.clone();
        let mut recorder = SessionRecorder::new(model, 20, 2, 7);
        recorder.init().unwrap();
        assert!(!recorder.step().unwrap().rendered);
        recorder.advance_time(1, Duration::from_millis(9));
        assert!(!recorder.step().unwrap().rendered);
        recorder.advance_time(2, Duration::from_millis(1));
        assert!(recorder.step().unwrap().rendered);
        assert!(!recorder.step().unwrap().rendered);
        assert_eq!(*seen.borrow(), vec![Event::Tick]);
        let trace = SessionTrace::from_jsonl_validated(&recorder.finish().to_jsonl()).unwrap();
        let model = observed_model(false, true);
        let replayed = model.seen.clone();
        let result = replay(model, &trace).unwrap();
        assert!(result.ok());
        assert_eq!(result.total_steps, 5);
        assert_eq!(result.total_frames, 2);
        assert_eq!(*replayed.borrow(), vec![Event::Tick]);
    }

    #[test]
    fn trace_requires_observed_boundaries_without_implicit_flush_or_stale_frames() {
        let mut recorder = SessionRecorder::new(new_counter(0), 20, 2, 7);
        recorder.init().unwrap();
        recorder.push_event(1, key_event('q')).unwrap();
        recorder.step().unwrap();
        let trace = recorder.finish();
        trace.validate().unwrap();
        let quit_idx = trace.records.len() - 2;
        for missing in [1, 2, quit_idx] {
            let mut changed = trace.clone();
            changed.records.remove(missing);
            assert!(changed.validate().is_err(), "missing record {missing}");
            assert!(replay(new_counter(0), &changed).is_err());
        }
        let mut duplicate = trace.clone();
        duplicate
            .records
            .insert(quit_idx, trace.records[quit_idx].clone());
        assert!(duplicate.validate().is_err());
        let mut reordered = trace.clone();
        reordered.records.swap(1, 2);
        assert!(reordered.validate().is_err());
        let mut bad_count = trace.clone();
        if let TraceRecord::Step { result, .. } = &mut bad_count.records[quit_idx] {
            result.events_pending = 1;
        }
        assert!(bad_count.validate().is_err());
        let mut after_quit = trace.clone();
        after_quit.records.insert(
            quit_idx + 1,
            TraceRecord::Input {
                ts_ns: 1,
                event: key_event('+'),
            },
        );
        assert!(after_quit.validate().is_err());

        let mut recorder = SessionRecorder::new(new_counter(0), 20, 2, 7);
        recorder.init().unwrap();
        recorder.push_event(1, key_event('+')).unwrap();
        assert!(recorder.finish().validate().is_err());
    }

    #[test]
    fn recorder_rejects_before_changing_trace_or_timestamp_and_replays_recovery() {
        let mut recorder = SessionRecorder::new(new_counter(0), 80, 24, 0);
        recorder.init().unwrap();
        for _ in 0..crate::WebEventSource::MAX_EVENTS {
            recorder.push_event(1, key_event('+')).unwrap();
        }
        let records_before = recorder.records.len();
        assert!(matches!(
            recorder.push_event(2, key_event('-')),
            Err(WebBackendError::InputQueueFull { .. })
        ));
        assert!(matches!(
            recorder.resize(3, 120, 40),
            Err(WebBackendError::InputQueueFull { .. })
        ));
        assert_eq!(recorder.records.len(), records_before);
        assert_eq!(recorder.current_ts_ns, 1);
        assert_eq!(recorder.program.size(), (80, 24));
        assert_eq!(
            recorder.step().unwrap().events_processed as usize,
            crate::WebEventSource::MAX_EVENTS
        );
        recorder.push_event(2, key_event('-')).unwrap();
        recorder.resize(3, 120, 40).unwrap();
        assert_eq!(recorder.step().unwrap().events_processed, 2);
        let trace = recorder.finish();
        trace.validate().unwrap();
        assert!(replay(new_counter(0), &trace).unwrap().ok());
    }

    #[test]
    fn replay_rejects_an_overfull_batch_without_inserting_unrecorded_steps() {
        let mut recorder = SessionRecorder::new(new_counter(0), 80, 24, 0);
        recorder.init().unwrap();
        let mut trace = recorder.finish();
        let summary_index = trace.records.len() - 1;
        trace.records.splice(
            summary_index..summary_index,
            (0..=crate::WebEventSource::MAX_EVENTS).map(|_| TraceRecord::Input {
                ts_ns: 0,
                event: key_event('+'),
            }),
        );
        trace.records.insert(
            trace.records.len() - 1,
            TraceRecord::Step {
                step_idx: 1,
                ts_ns: 0,
                init: false,
                clock: Duration::ZERO,
                result: StepResult {
                    running: false,
                    rendered: false,
                    events_processed: 0,
                    events_pending: crate::WebEventSource::MAX_EVENTS as u32 + 1,
                    frame_idx: 1,
                },
            },
        );
        trace.validate().unwrap();
        assert!(matches!(
            replay(new_counter(0), &trace),
            Err(ReplayError::Backend(WebBackendError::InputQueueFull {
                limit: crate::WebInputLimit::Events,
                ..
            }))
        ));
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
        assert_eq!(trace.records.len(), 4); // header + init step + frame + summary

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

        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(3_000_000, key_event('-'))
            .expect("key admission");
        rec.step().unwrap();

        rec.push_event(16_000_000, key_event('+'))
            .expect("key admission");
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

        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
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

        rec.resize(5_000_000, 40, 2).expect("resize admission");
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
            rec.push_event(i * 16_000_000, key_event('+'))
                .expect("key admission");
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

        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('q'))
            .expect("quit admission");
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
        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
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

    #[test]
    fn trace_validate_missing_summary_returns_typed_error() {
        let trace = SessionTrace {
            records: vec![TraceRecord::Header {
                seed: 0,
                cols: 80,
                rows: 24,
                profile: "modern".to_string(),
            }],
        };
        let result = trace.validate();
        assert_eq!(result, Err(TraceValidationError::MissingSummary));
    }

    #[test]
    fn trace_validate_summary_frame_count_mismatch_returns_typed_error() {
        let trace = SessionTrace {
            records: vec![
                TraceRecord::Header {
                    seed: 0,
                    cols: 80,
                    rows: 24,
                    profile: "modern".to_string(),
                },
                TraceRecord::Frame {
                    frame_idx: 0,
                    ts_ns: 0,
                    checksum: 0x1,
                    checksum_chain: 0x10,
                },
                TraceRecord::Summary {
                    total_frames: 2,
                    final_checksum_chain: 0x10,
                },
            ],
        };
        let result = trace.validate();
        assert_eq!(
            result,
            Err(TraceValidationError::SummaryFrameCountMismatch {
                expected: 1,
                actual: 2,
            })
        );
    }

    #[test]
    fn trace_validate_frame_index_gap_returns_typed_error() {
        let trace = SessionTrace {
            records: vec![
                TraceRecord::Header {
                    seed: 0,
                    cols: 80,
                    rows: 24,
                    profile: "modern".to_string(),
                },
                TraceRecord::Frame {
                    frame_idx: 1,
                    ts_ns: 0,
                    checksum: 0x1,
                    checksum_chain: 0x10,
                },
                TraceRecord::Summary {
                    total_frames: 1,
                    final_checksum_chain: 0x10,
                },
            ],
        };
        let result = trace.validate();
        assert_eq!(
            result,
            Err(TraceValidationError::FrameIndexMismatch {
                expected: 0,
                actual: 1,
            })
        );
    }

    #[test]
    fn trace_validate_timestamp_regression_returns_typed_error() {
        let trace = SessionTrace {
            records: vec![
                TraceRecord::Header {
                    seed: 0,
                    cols: 80,
                    rows: 24,
                    profile: "modern".to_string(),
                },
                TraceRecord::Tick { ts_ns: 20 },
                TraceRecord::Tick { ts_ns: 10 },
                TraceRecord::Summary {
                    total_frames: 0,
                    final_checksum_chain: 0,
                },
            ],
        };
        let result = trace.validate();
        assert_eq!(
            result,
            Err(TraceValidationError::TimestampRegression {
                previous: 20,
                current: 10,
                record_index: 2,
            })
        );
    }

    #[test]
    fn replay_validates_trace_before_execution() {
        let trace = SessionTrace {
            records: vec![TraceRecord::Header {
                seed: 0,
                cols: 80,
                rows: 24,
                profile: "modern".to_string(),
            }],
        };
        let result = replay(new_counter(0), &trace);
        assert_eq!(
            result,
            Err(ReplayError::InvalidTrace(
                TraceValidationError::MissingSummary
            ))
        );
    }

    // ---- Determinism: same input → same trace ----

    #[test]
    fn same_inputs_produce_same_trace_checksums() {
        fn record_session() -> SessionTrace {
            let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
            rec.init().unwrap();

            rec.push_event(1_000_000, key_event('+'))
                .expect("key admission");
            rec.push_event(2_000_000, key_event('+'))
                .expect("key admission");
            rec.push_event(3_000_000, key_event('-'))
                .expect("key admission");
            rec.step().unwrap();

            rec.push_event(16_000_000, key_event('+'))
                .expect("key admission");
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
        rec.push_event(1_000_000, mouse).expect("mouse admission");
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
        rec.push_event(1_000_000, paste).expect("paste admission");
        rec.step().unwrap();

        let trace = rec.finish();
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
    }

    #[test]
    fn focus_event_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, Event::Focus(true))
            .expect("focus admission");
        rec.push_event(2_000_000, Event::Focus(false))
            .expect("focus admission");
        rec.step().unwrap();

        let trace = rec.finish();
        let result = replay(new_counter(0), &trace).unwrap();
        assert!(result.ok());
    }

    #[test]
    fn ime_event_record_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        rec.push_event(1_000_000, Event::Ime(ImeEvent::start()))
            .expect("IME start admission");
        rec.push_event(2_000_000, Event::Ime(ImeEvent::update("你")))
            .expect("IME update admission");
        rec.push_event(3_000_000, Event::Ime(ImeEvent::commit("你好")))
            .expect("IME commit admission");
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

        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.step().unwrap();

        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
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
            total_steps: 5,
            running: true,
            unprocessed_events: Vec::new(),
            total_frames: 5,
            final_checksum_chain: 123,
            first_mismatch: None,
        };
        assert!(r.ok());
    }

    #[test]
    fn replay_result_not_ok_when_mismatch() {
        let r = ReplayResult {
            total_steps: 5,
            running: true,
            unprocessed_events: Vec::new(),
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
        let invalid = ReplayError::InvalidTrace(TraceValidationError::MissingSummary);
        assert_eq!(
            invalid.to_string(),
            "invalid trace: trace is missing summary"
        );
        let be = ReplayError::Backend(WebBackendError::Unsupported("test"));
        assert!(be.to_string().contains("test"));
    }

    // ---- JSONL serialization ----

    #[test]
    fn trace_record_header_to_jsonl() {
        let r = TraceRecord::Header {
            seed: 42,
            cols: 80,
            rows: 24,
            profile: "modern".to_string(),
        };
        let line = r.to_jsonl();
        assert!(line.contains("\"event\":\"trace_header\""));
        assert!(line.contains("\"schema_version\":\"golden-trace-v2\""));
        assert!(line.contains("\"seed\":42"));
        assert!(line.contains("\"cols\":80"));
        assert!(line.contains("\"rows\":24"));
        assert!(line.contains("\"profile\":\"modern\""));
    }

    #[test]
    fn trace_record_input_key_to_jsonl() {
        let r = TraceRecord::Input {
            ts_ns: 1_000_000,
            event: key_event('+'),
        };
        let line = r.to_jsonl();
        assert!(line.contains("\"event\":\"input\""));
        assert!(line.contains("\"ts_ns\":1000000"));
        assert!(line.contains("\"kind\":\"key\""));
        assert!(line.contains("\"code\":\"char:+\""));
    }

    #[test]
    fn trace_record_resize_to_jsonl() {
        let r = TraceRecord::Resize {
            ts_ns: 5_000_000,
            cols: 120,
            rows: 40,
        };
        let line = r.to_jsonl();
        assert!(line.contains("\"event\":\"resize\""));
        assert!(line.contains("\"cols\":120"));
        assert!(line.contains("\"rows\":40"));
    }

    #[test]
    fn trace_record_frame_to_jsonl() {
        let r = TraceRecord::Frame {
            frame_idx: 3,
            ts_ns: 48_000_000,
            checksum: 0xDEADBEEF,
            checksum_chain: 0xCAFEBABE,
        };
        let line = r.to_jsonl();
        assert!(line.contains("\"event\":\"frame\""));
        assert!(line.contains("\"frame_idx\":3"));
        assert!(line.contains("\"frame_hash\":\"00000000deadbeef\""));
        assert!(line.contains("\"checksum_chain\":\"00000000cafebabe\""));
    }

    #[test]
    fn trace_record_summary_to_jsonl() {
        let r = TraceRecord::Summary {
            total_frames: 10,
            final_checksum_chain: 0x1234567890ABCDEF,
        };
        let line = r.to_jsonl();
        assert!(line.contains("\"event\":\"trace_summary\""));
        assert!(line.contains("\"total_frames\":10"));
        assert!(line.contains("\"final_checksum_chain\":\"1234567890abcdef\""));
    }

    // ---- JSONL round-trip ----

    #[test]
    fn jsonl_round_trip_full_session() {
        // Record a session.
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 42);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
        rec.step().unwrap();
        let trace = rec.finish();

        // Serialize to JSONL.
        let jsonl = trace.to_jsonl();
        assert!(!jsonl.is_empty());

        // Deserialize back.
        let parsed = SessionTrace::from_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.records.len(), trace.records.len());
        assert_eq!(parsed.frame_count(), trace.frame_count());
        assert_eq!(parsed.final_checksum_chain(), trace.final_checksum_chain());
    }

    #[test]
    fn jsonl_round_trip_preserves_events() {
        let events = vec![
            key_event('+'),
            key_event('-'),
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: Modifiers::CTRL | Modifiers::SHIFT,
                kind: KeyEventKind::Press,
            }),
            Event::Key(KeyEvent {
                code: KeyCode::F(12),
                modifiers: Modifiers::ALT,
                kind: KeyEventKind::Repeat,
            }),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                x: 10,
                y: 5,
                modifiers: Modifiers::empty(),
            }),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                x: 0,
                y: 0,
                modifiers: Modifiers::CTRL,
            }),
            Event::Paste(PasteEvent::bracketed("hello world")),
            Event::Focus(true),
            Event::Focus(false),
            Event::Tick,
        ];

        for (i, event) in events.iter().enumerate() {
            let record = TraceRecord::Input {
                ts_ns: i as u64 * 1_000_000,
                event: event.clone(),
            };
            let jsonl = record.to_jsonl();
            let parsed = SessionTrace::from_jsonl(&jsonl).unwrap();
            let parsed_record = &parsed.records[0];
            let TraceRecord::Input {
                event: parsed_event,
                ..
            } = parsed_record
            else {
                unreachable!("expected Input record for event {i}");
            };

            assert_eq!(parsed_event, event, "event {i} round-trip failed: {jsonl}");
        }
    }

    #[test]
    fn jsonl_round_trip_with_resize() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.resize(5_000_000, 40, 2).expect("resize admission");
        rec.step().unwrap();
        let trace = rec.finish();

        let jsonl = trace.to_jsonl();
        let parsed = SessionTrace::from_jsonl(&jsonl).unwrap();

        // Replay parsed trace.
        let result = replay(new_counter(0), &parsed).unwrap();
        assert!(
            result.ok(),
            "parsed trace replay failed: {:?}",
            result.first_mismatch
        );
    }

    // ---- JSONL parsing errors ----

    #[test]
    fn step_jsonl_preserves_clock_and_requires_all_outcome_fields() {
        let record = TraceRecord::Step {
            step_idx: 4,
            ts_ns: 9,
            init: false,
            clock: Duration::new(u64::MAX, 999_999_999),
            result: StepResult {
                running: false,
                rendered: false,
                events_processed: 3,
                events_pending: 2,
                frame_idx: 2,
            },
        };
        let json = record.to_jsonl();
        assert_eq!(
            SessionTrace::from_jsonl(&json).unwrap().records,
            vec![record]
        );
        for field in [
            "step_idx",
            "ts_ns",
            "init",
            "clock_secs",
            "clock_subsec_nanos",
            "running",
            "rendered",
            "events_processed",
            "events_pending",
            "frame_idx",
        ] {
            let changed = json.replace(&format!("\"{field}\":"), "\"unknown\":");
            assert!(
                SessionTrace::from_jsonl(&changed).is_err(),
                "missing {field}"
            );
        }
        for (old, new) in [
            (
                "\"clock_subsec_nanos\":999999999",
                "\"clock_subsec_nanos\":1000000000",
            ),
            ("\"events_pending\":2", "\"events_pending\":4294967296"),
            ("\"events_processed\":3", "\"events_processed\":-1"),
            ("\"running\":false", "\"running\":null"),
            ("\"running\":false", "\"running\":falseX"),
            ("\"events_pending\":2", "\"events_pending\":2.5"),
            ("\"events_pending\":2", "\"events_pending\":2e3"),
            ("\"events_pending\":2", "\"nested\":{\"events_pending\":2}"),
            (
                "\"events_pending\":2",
                "\"events_pending\":2,\"events_pending\":2",
            ),
        ] {
            assert!(
                SessionTrace::from_jsonl(&json.replace(old, new)).is_err(),
                "invalid {new}"
            );
        }
    }

    #[test]
    fn v1_trace_cannot_invent_missing_execution_boundaries() {
        let line = r#"{"schema_version":"golden-trace-v1","event":"tick","ts_ns":0}"#;
        let error = SessionTrace::from_jsonl(line).unwrap_err();
        assert!(error.message.contains("migration required"));
    }

    #[test]
    fn text_payloads_with_braces_quotes_and_unicode_round_trip_in_quit_tail() {
        let text = "} { \\\" 🦀/\n\t\0\u{0008}\u{000c}";
        let tail = vec![
            Event::Paste(PasteEvent::bracketed(text)),
            Event::Ime(ImeEvent::commit(text)),
            Event::Clipboard(ftui_core::event::ClipboardEvent {
                content: text.to_owned(),
                source: ClipboardSource::Osc52,
            }),
        ];
        let mut recorder = SessionRecorder::new(new_counter(0), 20, 2, 7);
        recorder.init().unwrap();
        recorder.push_event(0, key_event('q')).unwrap();
        for event in &tail {
            recorder.push_event(0, event.clone()).unwrap();
        }
        recorder.step().unwrap();
        let jsonl = recorder.finish().to_jsonl();
        let escaped = jsonl
            .replace('🦀', r"\ud83e\udd80")
            .replace('/', r"\/")
            .replace(r"\u0008", r"\b")
            .replace(r"\u000c", r"\f");
        for input in [&jsonl, &escaped] {
            let trace = SessionTrace::from_jsonl_validated(input).unwrap();
            assert_eq!(
                replay(new_counter(0), &trace).unwrap().unprocessed_events,
                tail
            );
        }
    }

    #[test]
    fn malformed_text_escapes_are_rejected_without_changing_content() {
        for text in [r"\uZZZZ", r"\ud83e", r"\udd80", r"\ud83e\u0061", r"\q"] {
            let line = format!(
                r#"{{"schema_version":"{}","event":"input","ts_ns":0,"data":{{"kind":"paste","text":"{}"}}}}"#,
                SCHEMA_VERSION, text
            );
            assert!(SessionTrace::from_jsonl(&line).is_err(), "invalid {text}");
        }
    }

    #[test]
    fn chain_only_corruption_cannot_pass_replay_or_gate() {
        let mut recorder = SessionRecorder::new(new_counter(0), 20, 2, 7);
        recorder.init().unwrap();
        let mut trace = recorder.finish();
        for record in &mut trace.records {
            match record {
                TraceRecord::Frame { checksum_chain, .. } => *checksum_chain ^= 1,
                TraceRecord::Summary {
                    final_checksum_chain,
                    ..
                } => *final_checksum_chain ^= 1,
                _ => {}
            }
        }
        assert!(trace.validate().is_err());
        assert!(replay(new_counter(0), &trace).is_err());
        assert!(gate_trace(new_counter(0), &trace).is_err());
    }

    #[test]
    fn from_jsonl_empty_is_ok() {
        let trace = SessionTrace::from_jsonl("").unwrap();
        assert!(trace.records.is_empty());
    }

    #[test]
    fn from_jsonl_unknown_event_fails() {
        let line = r#"{"schema_version":"golden-trace-v2","event":"unknown_type","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("unknown event type"));
    }

    #[test]
    fn from_jsonl_missing_event_field_fails() {
        let line = r#"{"schema_version":"golden-trace-v2","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
    }

    #[test]
    fn from_jsonl_missing_schema_version_fails() {
        let line = r#"{"event":"tick","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("schema_version"));
    }

    #[test]
    fn from_jsonl_schema_matrix_current_writer_version_passes() {
        let line = format!(
            r#"{{"schema_version":"{}","event":"tick","ts_ns":0}}"#,
            SCHEMA_VERSION
        );
        let trace = SessionTrace::from_jsonl(&line).expect("matching schema should parse");
        assert_eq!(trace.records, vec![TraceRecord::Tick { ts_ns: 0 }]);
    }

    #[test]
    fn from_jsonl_schema_matrix_newer_writer_version_fails_with_migration_error() {
        let line = r#"{"schema_version":"golden-trace-v3","event":"tick","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
        let message = result.unwrap_err().message;
        assert!(message.contains("unsupported schema_version"));
        assert!(message.contains("migration required"));
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn schema_incompatibility_emits_compat_span_and_error_log() {
        let capture = TraceCaptureLayer::default();
        let subscriber =
            tracing_subscriber::registry().with(capture.clone().with_filter(LevelFilter::TRACE));
        let _guard = tracing::subscriber::set_default(subscriber);

        let line = r#"{"schema_version":"golden-trace-v3","event":"tick","ts_ns":0}"#;
        let err = SessionTrace::from_jsonl(line).expect_err("newer schema should fail");
        assert!(err.message.contains("migration required"));

        let spans = capture.spans.lock().expect("span capture lock");
        assert!(
            spans.iter().any(|name| name == "trace.compat_check"),
            "expected trace.compat_check span, got {spans:?}"
        );
        drop(spans);

        let events = capture.events.lock().expect("event capture lock");
        assert!(
            events
                .iter()
                .any(|event| event.contains("ERROR:trace schema version incompatible")),
            "expected incompatible schema ERROR log, got {events:?}"
        );
    }

    #[test]
    fn from_jsonl_validated_surfaces_validation_error_type() {
        let jsonl = TraceRecord::Header {
            seed: 0,
            cols: 80,
            rows: 24,
            profile: "modern".to_string(),
        }
        .to_jsonl();
        let result = SessionTrace::from_jsonl_validated(&jsonl);
        assert!(matches!(
            result,
            Err(TraceLoadError::Validation(
                TraceValidationError::MissingSummary
            ))
        ));
    }

    // ---- JSON helpers ----

    #[test]
    fn json_escape_round_trip() {
        let cases = [
            "hello",
            "with\"quotes",
            "back\\slash",
            "line\nbreak",
            "tab\there",
        ];
        for input in cases {
            let escaped = json_escape(input);
            let unescaped = json_unescape(&escaped).unwrap();
            assert_eq!(unescaped, input, "round-trip failed for: {input:?}");
        }
    }

    #[test]
    fn extract_str_basic() {
        let json = r#"{"name":"alice","age":30}"#;
        assert_eq!(extract_str(json, "name"), Some("alice"));
    }

    #[test]
    fn extract_u64_basic() {
        let json = r#"{"count":42,"name":"test"}"#;
        assert_eq!(extract_u64(json, "count"), Some(42));
    }

    #[test]
    fn extract_i64_basic() {
        let json = r#"{"dx":-12,"dy":7}"#;
        assert_eq!(extract_i64(json, "dx"), Some(-12));
        assert_eq!(extract_i64(json, "dy"), Some(7));
    }

    #[test]
    fn extract_bool_basic() {
        let json = r#"{"enabled":true,"disabled":false}"#;
        assert_eq!(extract_bool(json, "enabled"), Some(true));
        assert_eq!(extract_bool(json, "disabled"), Some(false));
    }

    #[test]
    fn extract_hex_u64_basic() {
        let json = r#"{"hash":"00000000deadbeef"}"#;
        assert_eq!(extract_hex_u64(json, "hash"), Some(0xDEADBEEF));
    }

    #[test]
    fn from_jsonl_parses_frankenterm_key_schema() {
        let down = parse_single_input_event(
            r#"{"kind":"key","phase":"down","code":"F12","mods":5,"repeat":false}"#,
        );
        assert_eq!(
            down,
            Event::Key(KeyEvent {
                code: KeyCode::F(12),
                modifiers: Modifiers::SHIFT | Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );

        let repeat = parse_single_input_event(
            r#"{"kind":"key","phase":"down","code":"a","mods":0,"repeat":true}"#,
        );
        assert_eq!(
            repeat,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Repeat,
            })
        );

        let release = parse_single_input_event(
            r#"{"kind":"key","phase":"up","code":"Enter","mods":0,"repeat":false}"#,
        );
        assert_eq!(
            release,
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Release,
            })
        );
    }

    #[test]
    fn key_event_json_round_trip_unescapes_code() {
        let quote_key = Event::Key(KeyEvent {
            code: KeyCode::Char('"'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let quote_json = event_to_json(&quote_key);
        let parsed_quote = parse_event_json(&quote_json).expect("quote key should parse");
        assert_eq!(parsed_quote, quote_key);

        let slash_key = Event::Key(KeyEvent {
            code: KeyCode::Char('\\'),
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        });
        let slash_json = event_to_json(&slash_key);
        let parsed_slash = parse_event_json(&slash_json).expect("slash key should parse");
        assert_eq!(parsed_slash, slash_key);
    }

    #[test]
    fn from_jsonl_parses_frankenterm_mouse_and_wheel_schema() {
        let mouse = parse_single_input_event(
            r#"{"kind":"mouse","phase":"drag","button":2,"x":7,"y":9,"mods":3}"#,
        );
        assert_eq!(
            mouse,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Right),
                x: 7,
                y: 9,
                modifiers: Modifiers::SHIFT | Modifiers::ALT,
            })
        );

        let wheel =
            parse_single_input_event(r#"{"kind":"wheel","x":4,"y":6,"dx":0,"dy":-2,"mods":4}"#);
        assert_eq!(
            wheel,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                x: 4,
                y: 6,
                modifiers: Modifiers::CTRL,
            })
        );
    }

    #[test]
    fn from_jsonl_parses_frankenterm_paste_focus_and_composition_aliases() {
        let paste = parse_single_input_event(r#"{"kind":"paste","data":"hello\nworld"}"#);
        assert_eq!(paste, Event::Paste(PasteEvent::new("hello\nworld", true)));

        let focus = parse_single_input_event(r#"{"kind":"focus","focused":false}"#);
        assert_eq!(focus, Event::Focus(false));

        let composition_update =
            parse_single_input_event(r#"{"kind":"composition","phase":"update","data":"你"}"#);
        assert_eq!(
            composition_update,
            Event::Ime(ImeEvent::new(ImePhase::Update, "你"))
        );

        let composition_end =
            parse_single_input_event(r#"{"kind":"composition","phase":"end","data":"你好"}"#);
        assert_eq!(
            composition_end,
            Event::Ime(ImeEvent::new(ImePhase::Commit, "你好"))
        );
    }

    // ---- Golden Gate API ----

    #[test]
    fn gate_trace_passes_on_correct_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.step().unwrap();
        let trace = rec.finish();

        let report = gate_trace(new_counter(0), &trace).unwrap();
        assert!(report.passed);
        assert_eq!(report.total_frames, 2);
        assert!(report.diff.is_none());
        assert!(report.format().starts_with("PASS"));
    }

    #[test]
    fn gate_trace_fails_with_actionable_diff() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
        rec.step().unwrap();
        let trace = rec.finish();

        // Replay with different initial state.
        let report = gate_trace(new_counter(5), &trace).unwrap();
        assert!(!report.passed);
        assert!(report.diff.is_some());

        let diff = report.diff.as_ref().unwrap();
        assert_eq!(diff.frame_idx, 0); // First frame mismatch (init).

        let formatted = report.format();
        assert!(formatted.starts_with("FAIL"));
        assert!(formatted.contains("frame 0"));
    }

    #[test]
    fn gate_trace_diff_has_event_context() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'))
            .expect("key admission");
        rec.push_event(2_000_000, key_event('+'))
            .expect("key admission");
        rec.step().unwrap();
        rec.push_event(3_000_000, key_event('-'))
            .expect("key admission");
        rec.step().unwrap();
        let trace = rec.finish();

        // Tamper with the trace: change a frame checksum.
        let mut tampered = trace.clone();
        let mut chain = 0;
        for record in &mut tampered.records {
            match record {
                TraceRecord::Frame {
                    frame_idx,
                    checksum,
                    checksum_chain,
                    ..
                } => {
                    if *frame_idx == 2 {
                        *checksum = 0xBAD;
                    }
                    chain = fnv1a64_pair(chain, *checksum);
                    *checksum_chain = chain;
                }
                TraceRecord::Summary {
                    final_checksum_chain,
                    ..
                } => *final_checksum_chain = chain,
                _ => {}
            }
        }
        // Preserve structural integrity so this still tests an actual model
        // checksum mismatch, independently of the malformed-chain rejection.
        tampered.validate().unwrap();

        let report = gate_trace(new_counter(0), &tampered).unwrap();
        assert!(!report.passed);
        let diff = report.diff.unwrap();
        assert_eq!(diff.frame_idx, 2);
        assert!(diff.event_idx > 0); // Events were processed before frame 2.
    }

    // ---- JSONL → replay integration ----

    #[test]
    fn jsonl_serialize_parse_replay_round_trip() {
        // Full pipeline: record → JSONL → parse → replay → verify.
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();

        for i in 0..3 {
            rec.push_event(i * 16_000_000, key_event('+'))
                .expect("key admission");
            rec.step().unwrap();
        }
        let original_trace = rec.finish();

        // Serialize.
        let jsonl = original_trace.to_jsonl();

        // Parse.
        let parsed_trace = SessionTrace::from_jsonl(&jsonl).unwrap();

        // Replay parsed trace.
        let result = replay(new_counter(0), &parsed_trace).unwrap();
        assert!(
            result.ok(),
            "JSONL round-trip replay failed: {:?}",
            result.first_mismatch
        );
        assert_eq!(result.total_frames, original_trace.frame_count());
        assert_eq!(
            result.final_checksum_chain,
            original_trace.final_checksum_chain().unwrap()
        );
    }

    // ---- TraceParseError ----

    #[test]
    fn trace_parse_error_display() {
        let e = TraceParseError {
            line: 5,
            message: "bad field".to_string(),
        };
        assert_eq!(e.to_string(), "line 5: bad field");
    }
}
