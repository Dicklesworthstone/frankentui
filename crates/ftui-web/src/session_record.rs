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

use ftui_core::event::{
    ClipboardEvent, ClipboardSource, Event, KeyCode, KeyEvent, KeyEventKind, Modifiers,
    MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};
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

    /// Validate structural invariants for a recorded trace.
    ///
    /// This checks:
    /// - header exists and is the first record
    /// - summary exists and is the last record
    /// - frame indices are contiguous and start at zero
    /// - summary totals/chains match frame records
    pub fn validate(&self) -> Result<(), TraceValidationError> {
        if self.records.is_empty() {
            return Err(TraceValidationError::EmptyTrace);
        }

        let mut header_count: usize = 0;
        let mut summary: Option<(usize, u64, u64)> = None;
        let mut expected_frame_idx: u64 = 0;
        let mut frame_count: u64 = 0;
        let mut last_checksum_chain: u64 = 0;

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
                    checksum_chain,
                    ..
                } => {
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
                TraceRecord::Input { .. } | TraceRecord::Resize { .. } | TraceRecord::Tick { .. } => {
                    if summary.is_some() {
                        let summary_idx = summary.map(|(i, _, _)| i).unwrap_or_default();
                        return Err(TraceValidationError::SummaryNotLast {
                            summary_index: summary_idx,
                        });
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
            return Err(TraceValidationError::SummaryNotLast { summary_index: summary_idx });
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
    /// The trace violates structural invariants.
    InvalidTrace(TraceValidationError),
    /// A backend error occurred during replay.
    Backend(WebBackendError),
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingHeader => write!(f, "trace missing header record"),
            Self::InvalidTrace(e) => write!(f, "invalid trace: {e}"),
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
    trace.validate().map_err(ReplayError::InvalidTrace)?;

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

impl TraceRecord {
    /// Serialize this record as a golden-trace-v1 JSONL line.
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
    /// Serialize the entire trace as a golden-trace-v1 JSONL string.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for record in &self.records {
            out.push_str(&record.to_jsonl());
            out.push('\n');
        }
        out
    }

    /// Parse a golden-trace-v1 JSONL string into a `SessionTrace`.
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

    /// Parse and validate a golden-trace-v1 JSONL payload.
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
    EmptyTrace,
    MissingHeader,
    HeaderNotFirst,
    MultipleHeaders,
    MissingSummary,
    MultipleSummaries,
    SummaryNotLast { summary_index: usize },
    FrameIndexMismatch { expected: u64, actual: u64 },
    SummaryFrameCountMismatch { expected: u64, actual: u64 },
    SummaryChecksumChainMismatch { expected: u64, actual: u64 },
}

impl core::fmt::Display for TraceValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
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

fn extract_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\":\"", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = &json[start..];
    // Find closing quote, handling escapes.
    let mut i = 0;
    let bytes = rest.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2; // Skip escaped char.
            continue;
        }
        if bytes[i] == b'"' {
            return Some(&rest[..i]);
        }
        i += 1;
    }
    None
}

fn extract_u64(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\":", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = json[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_u16(json: &str, key: &str) -> Option<u16> {
    extract_u64(json, key).and_then(|v| u16::try_from(v).ok())
}

fn extract_bool(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\":", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = json[start..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn extract_hex_u64(json: &str, key: &str) -> Option<u64> {
    let s = extract_str(json, key)?;
    u64::from_str_radix(s, 16).ok()
}

fn extract_object<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\":", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = json[start..].trim_start();
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(cp)
                    {
                        out.push(c);
                    }
                }
                Some(c) => {
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_trace_line(line: &str, line_num: usize) -> Result<TraceRecord, TraceParseError> {
    let err = |msg: &str| TraceParseError {
        line: line_num,
        message: msg.to_string(),
    };

    let schema_version = extract_str(line, "schema_version")
        .ok_or_else(|| err("missing \"schema_version\" field"))?;
    if schema_version != SCHEMA_VERSION {
        return Err(err(&format!("unsupported schema_version: {schema_version}")));
    }

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
            let code = parse_key_code(code_str)?;
            let mods_bits = extract_u64(data, "modifiers").unwrap_or(0) as u8;
            let modifiers = Modifiers::from_bits(mods_bits).unwrap_or_else(Modifiers::empty);
            let event_kind_str = extract_str(data, "event_kind").unwrap_or("press");
            let event_kind = match event_kind_str {
                "press" => KeyEventKind::Press,
                "repeat" => KeyEventKind::Repeat,
                "release" => KeyEventKind::Release,
                _ => KeyEventKind::Press,
            };
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind: event_kind,
            }))
        }
        "mouse" => {
            let mouse_kind_str = extract_str(data, "mouse_kind").ok_or("missing mouse_kind")?;
            let mouse_kind = parse_mouse_event_kind(mouse_kind_str)?;
            let x = extract_u16(data, "x").unwrap_or(0);
            let y = extract_u16(data, "y").unwrap_or(0);
            let mods_bits = extract_u64(data, "modifiers").unwrap_or(0) as u8;
            let modifiers = Modifiers::from_bits(mods_bits).unwrap_or_else(Modifiers::empty);
            Ok(Event::Mouse(MouseEvent {
                kind: mouse_kind,
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
                .map(json_unescape)
                .unwrap_or_default();
            let bracketed = extract_bool(data, "bracketed").unwrap_or(true);
            Ok(Event::Paste(PasteEvent::new(text, bracketed)))
        }
        "focus" => {
            let gained = extract_bool(data, "gained").unwrap_or(true);
            Ok(Event::Focus(gained))
        }
        "clipboard" => {
            let content = extract_str(data, "content")
                .map(json_unescape)
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

fn parse_key_code(s: &str) -> Result<KeyCode, String> {
    if let Some(rest) = s.strip_prefix("char:") {
        let ch = rest.chars().next().ok_or("empty char code")?;
        return Ok(KeyCode::Char(ch));
    }
    if let Some(rest) = s.strip_prefix("f:") {
        let n: u8 = rest.parse().map_err(|_| "invalid F-key number")?;
        return Ok(KeyCode::F(n));
    }
    match s {
        "enter" => Ok(KeyCode::Enter),
        "escape" => Ok(KeyCode::Escape),
        "backspace" => Ok(KeyCode::Backspace),
        "tab" => Ok(KeyCode::Tab),
        "backtab" => Ok(KeyCode::BackTab),
        "delete" => Ok(KeyCode::Delete),
        "insert" => Ok(KeyCode::Insert),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "pageup" => Ok(KeyCode::PageUp),
        "pagedown" => Ok(KeyCode::PageDown),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "null" => Ok(KeyCode::Null),
        "media_play_pause" => Ok(KeyCode::MediaPlayPause),
        "media_stop" => Ok(KeyCode::MediaStop),
        "media_next" => Ok(KeyCode::MediaNextTrack),
        "media_prev" => Ok(KeyCode::MediaPrevTrack),
        other => Err(format!("unknown key code: {other}")),
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
        total_frames: result.total_frames,
        expected_frames: frame_checksums.len() as u64,
        final_checksum_chain: result.final_checksum_chain,
        diff,
    })
}

/// Report from a golden trace gate validation.
#[derive(Debug, Clone)]
pub struct GateReport {
    /// Whether all frame checksums matched.
    pub passed: bool,
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
                "PASS: {}/{} frames verified, final_chain={:016x}",
                self.total_frames, self.expected_frames, self.final_checksum_chain
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
        assert!(line.contains("\"schema_version\":\"golden-trace-v1\""));
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
        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('+'));
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
            if let TraceRecord::Input {
                event: parsed_event,
                ..
            } = parsed_record
            {
                assert_eq!(parsed_event, event, "event {i} round-trip failed: {jsonl}");
            } else {
                panic!("expected Input record for event {i}");
            }
        }
    }

    #[test]
    fn jsonl_round_trip_with_resize() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.resize(5_000_000, 40, 2);
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
    fn from_jsonl_empty_is_ok() {
        let trace = SessionTrace::from_jsonl("").unwrap();
        assert!(trace.records.is_empty());
    }

    #[test]
    fn from_jsonl_unknown_event_fails() {
        let line = r#"{"schema_version":"golden-trace-v1","event":"unknown_type","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("unknown event type"));
    }

    #[test]
    fn from_jsonl_missing_event_field_fails() {
        let line = r#"{"schema_version":"golden-trace-v1","ts_ns":0}"#;
        let result = SessionTrace::from_jsonl(line);
        assert!(result.is_err());
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
            let unescaped = json_unescape(&escaped);
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

    // ---- Golden Gate API ----

    #[test]
    fn gate_trace_passes_on_correct_replay() {
        let mut rec = SessionRecorder::new(new_counter(0), 20, 1, 0);
        rec.init().unwrap();
        rec.push_event(1_000_000, key_event('+'));
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
        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('+'));
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
        rec.push_event(1_000_000, key_event('+'));
        rec.push_event(2_000_000, key_event('+'));
        rec.step().unwrap();
        rec.push_event(3_000_000, key_event('-'));
        rec.step().unwrap();
        let trace = rec.finish();

        // Tamper with the trace: change a frame checksum.
        let mut tampered = trace.clone();
        for record in &mut tampered.records {
            if let TraceRecord::Frame {
                frame_idx,
                checksum,
                ..
            } = record
                && *frame_idx == 2
            {
                *checksum = 0xBAD;
            }
        }

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
            rec.push_event(i * 16_000_000, key_event('+'));
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
