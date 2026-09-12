#![forbid(unsafe_code)]

//! Event trace recording and replay for deterministic debugging (bd-3mjjt.2,
//! bd-3mjjt.4).
//!
//! Records all external events (keyboard, mouse, resize, paste, IME, focus,
//! clipboard) and Bayesian evidence ledger entries with monotonic nanosecond
//! timestamps to a gzip-compressed JSONL file. [`EventReplayer`] reads the
//! trace back and feeds events in exact order with timing information.
//!
//! Evidence capture (bd-3mjjt.4): During recording, [`EventTraceWriter::record_evidence`]
//! captures [`EvidenceEntry`](crate::unified_evidence::EvidenceEntry) decisions.
//! During replay, [`EvidenceVerifier`] compares replayed evidence against the
//! recording to validate that all Bayesian decisions are deterministic given
//! the same input sequence.
//!
//! # Format
//!
//! Each line is a JSON object with `{event, ts_ns, ...payload}`.
//! The first line is always a `trace_header`, the last a `trace_summary`.
//!
//! # Storage
//!
//! Files use gzip compression (`.jsonl.gz`) by default. Uncompressed
//! `.jsonl` is also supported for debugging.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::event_trace::{EventTraceWriter, EventTraceReader, EventReplayer};
//! use std::path::Path;
//!
//! // Record
//! let mut writer = EventTraceWriter::gzip("trace.jsonl.gz", "my_session", (80, 24))?;
//! writer.record(&some_event, 100_000)?;
//! writer.finish()?;
//!
//! // Replay
//! let reader = EventTraceReader::open("trace.jsonl.gz")?;
//! let mut replayer = EventReplayer::new(reader.records()?);
//! while let Some((event, ts_ns)) = replayer.next_event() {
//!     // feed event into simulator
//! }
//! ```

use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use ftui_core::event::{
    ClipboardEvent, ClipboardSource, Event, ImeEvent, ImePhase, KeyCode, KeyEvent, KeyEventKind,
    Modifiers, MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};
use serde::{Deserialize, Serialize};

use crate::unified_evidence::{DecisionDomain, EvidenceEntry};

// ---------------------------------------------------------------------------
// Schema version
// ---------------------------------------------------------------------------

/// Current schema version for event trace files.
pub const SCHEMA_VERSION: &str = "event-trace-v1";

// ---------------------------------------------------------------------------
// Serializable trace records
// ---------------------------------------------------------------------------

/// A single record in an event trace JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event")]
pub enum TraceRecord {
    /// Header record (first line).
    #[serde(rename = "trace_header")]
    Header {
        schema_version: String,
        session_name: String,
        terminal_size: (u16, u16),
        #[serde(skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
    },

    /// Keyboard event.
    #[serde(rename = "key")]
    Key {
        ts_ns: u64,
        code: SerKeyCode,
        modifiers: u8,
        kind: SerKeyEventKind,
    },

    /// Mouse event.
    #[serde(rename = "mouse")]
    Mouse {
        ts_ns: u64,
        kind: SerMouseEventKind,
        x: u16,
        y: u16,
        modifiers: u8,
    },

    /// Terminal resize.
    #[serde(rename = "resize")]
    Resize { ts_ns: u64, cols: u16, rows: u16 },

    /// Paste event.
    #[serde(rename = "paste")]
    Paste {
        ts_ns: u64,
        text: String,
        bracketed: bool,
    },

    /// IME composition event.
    #[serde(rename = "ime")]
    Ime {
        ts_ns: u64,
        phase: SerImePhase,
        text: String,
    },

    /// Focus gained or lost.
    #[serde(rename = "focus")]
    Focus { ts_ns: u64, gained: bool },

    /// Clipboard content received.
    #[serde(rename = "clipboard")]
    Clipboard {
        ts_ns: u64,
        content: String,
        source: SerClipboardSource,
    },

    /// Tick event.
    #[serde(rename = "tick")]
    Tick { ts_ns: u64 },

    /// Frame timing snapshot (optional, for system-time-at-frame recording).
    #[serde(rename = "frame_time")]
    FrameTime {
        ts_ns: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        render_us: Option<u64>,
    },

    /// RNG seed capture.
    #[serde(rename = "rng_seed")]
    RngSeed { ts_ns: u64, seed: u64 },

    /// Evidence ledger entry from a Bayesian decision point (bd-3mjjt.4).
    #[serde(rename = "evidence")]
    Evidence { ts_ns: u64, entry: SerEvidenceEntry },

    /// Summary record (last line).
    #[serde(rename = "trace_summary")]
    Summary {
        total_events: u64,
        total_duration_ns: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_evidence: Option<u64>,
    },
}

// ---------------------------------------------------------------------------
// Serializable sub-types
// ---------------------------------------------------------------------------

/// Serializable key code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value")]
pub enum SerKeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    BackTab,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
    Null,
    MediaPlayPause,
    MediaStop,
    MediaNextTrack,
    MediaPrevTrack,
}

/// Serializable key event kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerKeyEventKind {
    Press,
    Repeat,
    Release,
}

/// Serializable mouse event kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "button")]
pub enum SerMouseEventKind {
    Down(SerMouseButton),
    Up(SerMouseButton),
    Drag(SerMouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

/// Serializable mouse button.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerMouseButton {
    Left,
    Right,
    Middle,
}

/// Serializable IME phase.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerImePhase {
    Start,
    Update,
    Commit,
    Cancel,
}

/// Serializable clipboard source.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerClipboardSource {
    Osc52,
    Unknown,
}

// ---------------------------------------------------------------------------
// Serializable evidence types (bd-3mjjt.4)
// ---------------------------------------------------------------------------

/// Serializable decision domain for event traces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerDecisionDomain {
    #[serde(rename = "diff_strategy")]
    DiffStrategy,
    #[serde(rename = "resize_coalescing")]
    ResizeCoalescing,
    #[serde(rename = "frame_budget")]
    FrameBudget,
    #[serde(rename = "degradation")]
    Degradation,
    #[serde(rename = "voi_sampling")]
    VoiSampling,
    #[serde(rename = "hint_ranking")]
    HintRanking,
    #[serde(rename = "palette_scoring")]
    PaletteScoring,
}

impl SerDecisionDomain {
    /// Convert from a runtime [`DecisionDomain`].
    pub fn from_domain(d: DecisionDomain) -> Self {
        match d {
            DecisionDomain::DiffStrategy => Self::DiffStrategy,
            DecisionDomain::ResizeCoalescing => Self::ResizeCoalescing,
            DecisionDomain::FrameBudget => Self::FrameBudget,
            DecisionDomain::Degradation => Self::Degradation,
            DecisionDomain::VoiSampling => Self::VoiSampling,
            DecisionDomain::HintRanking => Self::HintRanking,
            DecisionDomain::PaletteScoring => Self::PaletteScoring,
        }
    }

    /// Convert back to a runtime [`DecisionDomain`].
    pub fn into_domain(self) -> DecisionDomain {
        match self {
            Self::DiffStrategy => DecisionDomain::DiffStrategy,
            Self::ResizeCoalescing => DecisionDomain::ResizeCoalescing,
            Self::FrameBudget => DecisionDomain::FrameBudget,
            Self::Degradation => DecisionDomain::Degradation,
            Self::VoiSampling => DecisionDomain::VoiSampling,
            Self::HintRanking => DecisionDomain::HintRanking,
            Self::PaletteScoring => DecisionDomain::PaletteScoring,
        }
    }
}

/// Serializable evidence term for event traces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerEvidenceTerm {
    pub label: String,
    pub bayes_factor: f64,
}

/// Serializable evidence entry for event traces.
///
/// Mirrors [`EvidenceEntry`](crate::unified_evidence::EvidenceEntry) with
/// owned strings instead of `&'static str`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerEvidenceEntry {
    pub decision_id: u64,
    pub domain: SerDecisionDomain,
    pub log_posterior: f64,
    pub evidence: Vec<SerEvidenceTerm>,
    pub action: String,
    pub loss_avoided: f64,
    pub confidence_interval: (f64, f64),
}

impl SerEvidenceEntry {
    /// Convert from a runtime [`EvidenceEntry`](crate::unified_evidence::EvidenceEntry).
    pub fn from_entry(e: &EvidenceEntry) -> Self {
        let evidence = e
            .top_evidence
            .iter()
            .flatten()
            .map(|t| SerEvidenceTerm {
                label: t.label.to_string(),
                bayes_factor: t.bayes_factor,
            })
            .collect();

        Self {
            decision_id: e.decision_id,
            domain: SerDecisionDomain::from_domain(e.domain),
            log_posterior: e.log_posterior,
            evidence,
            action: e.action.to_string(),
            loss_avoided: e.loss_avoided,
            confidence_interval: e.confidence_interval,
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion: ftui Event <-> TraceRecord
// ---------------------------------------------------------------------------

impl TraceRecord {
    /// Convert an ftui [`Event`] into a [`TraceRecord`] at the given timestamp.
    #[must_use]
    pub fn from_event(event: &Event, ts_ns: u64) -> Self {
        match event {
            Event::Key(ke) => TraceRecord::Key {
                ts_ns,
                code: SerKeyCode::from_key_code(ke.code),
                modifiers: ke.modifiers.bits(),
                kind: SerKeyEventKind::from_kind(ke.kind),
            },
            Event::Mouse(me) => TraceRecord::Mouse {
                ts_ns,
                kind: SerMouseEventKind::from_kind(me.kind),
                x: me.x,
                y: me.y,
                modifiers: me.modifiers.bits(),
            },
            Event::Resize { width, height } => TraceRecord::Resize {
                ts_ns,
                cols: *width,
                rows: *height,
            },
            Event::Paste(pe) => TraceRecord::Paste {
                ts_ns,
                text: pe.text.clone(),
                bracketed: pe.bracketed,
            },
            Event::Ime(ie) => TraceRecord::Ime {
                ts_ns,
                phase: SerImePhase::from_phase(ie.phase),
                text: ie.text.clone(),
            },
            Event::Focus(gained) => TraceRecord::Focus {
                ts_ns,
                gained: *gained,
            },
            Event::Clipboard(ce) => TraceRecord::Clipboard {
                ts_ns,
                content: ce.content.clone(),
                source: SerClipboardSource::from_source(ce.source),
            },
            Event::Tick => TraceRecord::Tick { ts_ns },
        }
    }

    /// Convert this record back to an ftui [`Event`], if it represents one.
    ///
    /// Returns `None` for header, summary, frame_time, and rng_seed records.
    #[must_use]
    pub fn to_event(&self) -> Option<Event> {
        match self {
            TraceRecord::Key {
                code,
                modifiers,
                kind,
                ..
            } => Some(Event::Key(KeyEvent {
                code: code.to_key_code(),
                modifiers: Modifiers::from_bits_truncate(*modifiers),
                kind: kind.into_kind(),
            })),
            TraceRecord::Mouse {
                kind,
                x,
                y,
                modifiers,
                ..
            } => Some(Event::Mouse(MouseEvent {
                kind: kind.into_kind(),
                x: *x,
                y: *y,
                modifiers: Modifiers::from_bits_truncate(*modifiers),
            })),
            TraceRecord::Resize { cols, rows, .. } => Some(Event::Resize {
                width: *cols,
                height: *rows,
            }),
            TraceRecord::Paste {
                text, bracketed, ..
            } => Some(Event::Paste(PasteEvent {
                text: text.clone(),
                bracketed: *bracketed,
            })),
            TraceRecord::Ime { phase, text, .. } => Some(Event::Ime(ImeEvent {
                phase: phase.into_phase(),
                text: text.clone(),
            })),
            TraceRecord::Focus { gained, .. } => Some(Event::Focus(*gained)),
            TraceRecord::Clipboard {
                content, source, ..
            } => Some(Event::Clipboard(ClipboardEvent {
                content: content.clone(),
                source: source.into_source(),
            })),
            TraceRecord::Tick { .. } => Some(Event::Tick),
            TraceRecord::Header { .. }
            | TraceRecord::Summary { .. }
            | TraceRecord::FrameTime { .. }
            | TraceRecord::RngSeed { .. }
            | TraceRecord::Evidence { .. } => None,
        }
    }

    /// Get the timestamp in nanoseconds, if this record has one.
    #[must_use]
    pub fn ts_ns(&self) -> Option<u64> {
        match self {
            TraceRecord::Key { ts_ns, .. }
            | TraceRecord::Mouse { ts_ns, .. }
            | TraceRecord::Resize { ts_ns, .. }
            | TraceRecord::Paste { ts_ns, .. }
            | TraceRecord::Ime { ts_ns, .. }
            | TraceRecord::Focus { ts_ns, .. }
            | TraceRecord::Clipboard { ts_ns, .. }
            | TraceRecord::Tick { ts_ns, .. }
            | TraceRecord::FrameTime { ts_ns, .. }
            | TraceRecord::RngSeed { ts_ns, .. }
            | TraceRecord::Evidence { ts_ns, .. } => Some(*ts_ns),
            TraceRecord::Header { .. } | TraceRecord::Summary { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-type conversions
// ---------------------------------------------------------------------------

impl SerKeyCode {
    fn from_key_code(kc: KeyCode) -> Self {
        match kc {
            KeyCode::Char(c) => SerKeyCode::Char(c),
            KeyCode::Enter => SerKeyCode::Enter,
            KeyCode::Escape => SerKeyCode::Escape,
            KeyCode::Backspace => SerKeyCode::Backspace,
            KeyCode::Tab => SerKeyCode::Tab,
            KeyCode::BackTab => SerKeyCode::BackTab,
            KeyCode::Delete => SerKeyCode::Delete,
            KeyCode::Insert => SerKeyCode::Insert,
            KeyCode::Home => SerKeyCode::Home,
            KeyCode::End => SerKeyCode::End,
            KeyCode::PageUp => SerKeyCode::PageUp,
            KeyCode::PageDown => SerKeyCode::PageDown,
            KeyCode::Up => SerKeyCode::Up,
            KeyCode::Down => SerKeyCode::Down,
            KeyCode::Left => SerKeyCode::Left,
            KeyCode::Right => SerKeyCode::Right,
            KeyCode::F(n) => SerKeyCode::F(n),
            KeyCode::Null => SerKeyCode::Null,
            KeyCode::MediaPlayPause => SerKeyCode::MediaPlayPause,
            KeyCode::MediaStop => SerKeyCode::MediaStop,
            KeyCode::MediaNextTrack => SerKeyCode::MediaNextTrack,
            KeyCode::MediaPrevTrack => SerKeyCode::MediaPrevTrack,
        }
    }

    fn to_key_code(&self) -> KeyCode {
        match self {
            SerKeyCode::Char(c) => KeyCode::Char(*c),
            SerKeyCode::Enter => KeyCode::Enter,
            SerKeyCode::Escape => KeyCode::Escape,
            SerKeyCode::Backspace => KeyCode::Backspace,
            SerKeyCode::Tab => KeyCode::Tab,
            SerKeyCode::BackTab => KeyCode::BackTab,
            SerKeyCode::Delete => KeyCode::Delete,
            SerKeyCode::Insert => KeyCode::Insert,
            SerKeyCode::Home => KeyCode::Home,
            SerKeyCode::End => KeyCode::End,
            SerKeyCode::PageUp => KeyCode::PageUp,
            SerKeyCode::PageDown => KeyCode::PageDown,
            SerKeyCode::Up => KeyCode::Up,
            SerKeyCode::Down => KeyCode::Down,
            SerKeyCode::Left => KeyCode::Left,
            SerKeyCode::Right => KeyCode::Right,
            SerKeyCode::F(n) => KeyCode::F(*n),
            SerKeyCode::Null => KeyCode::Null,
            SerKeyCode::MediaPlayPause => KeyCode::MediaPlayPause,
            SerKeyCode::MediaStop => KeyCode::MediaStop,
            SerKeyCode::MediaNextTrack => KeyCode::MediaNextTrack,
            SerKeyCode::MediaPrevTrack => KeyCode::MediaPrevTrack,
        }
    }
}

impl SerKeyEventKind {
    fn from_kind(k: KeyEventKind) -> Self {
        match k {
            KeyEventKind::Press => SerKeyEventKind::Press,
            KeyEventKind::Repeat => SerKeyEventKind::Repeat,
            KeyEventKind::Release => SerKeyEventKind::Release,
        }
    }

    fn into_kind(self) -> KeyEventKind {
        match self {
            SerKeyEventKind::Press => KeyEventKind::Press,
            SerKeyEventKind::Repeat => KeyEventKind::Repeat,
            SerKeyEventKind::Release => KeyEventKind::Release,
        }
    }
}

impl SerMouseEventKind {
    fn from_kind(k: MouseEventKind) -> Self {
        match k {
            MouseEventKind::Down(b) => SerMouseEventKind::Down(SerMouseButton::from_button(b)),
            MouseEventKind::Up(b) => SerMouseEventKind::Up(SerMouseButton::from_button(b)),
            MouseEventKind::Drag(b) => SerMouseEventKind::Drag(SerMouseButton::from_button(b)),
            MouseEventKind::Moved => SerMouseEventKind::Moved,
            MouseEventKind::ScrollUp => SerMouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown => SerMouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft => SerMouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight => SerMouseEventKind::ScrollRight,
        }
    }

    fn into_kind(self) -> MouseEventKind {
        match self {
            SerMouseEventKind::Down(b) => MouseEventKind::Down(b.into_button()),
            SerMouseEventKind::Up(b) => MouseEventKind::Up(b.into_button()),
            SerMouseEventKind::Drag(b) => MouseEventKind::Drag(b.into_button()),
            SerMouseEventKind::Moved => MouseEventKind::Moved,
            SerMouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
            SerMouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
            SerMouseEventKind::ScrollLeft => MouseEventKind::ScrollLeft,
            SerMouseEventKind::ScrollRight => MouseEventKind::ScrollRight,
        }
    }
}

impl SerMouseButton {
    fn from_button(b: MouseButton) -> Self {
        match b {
            MouseButton::Left => SerMouseButton::Left,
            MouseButton::Right => SerMouseButton::Right,
            MouseButton::Middle => SerMouseButton::Middle,
        }
    }

    fn into_button(self) -> MouseButton {
        match self {
            SerMouseButton::Left => MouseButton::Left,
            SerMouseButton::Right => MouseButton::Right,
            SerMouseButton::Middle => MouseButton::Middle,
        }
    }
}

impl SerImePhase {
    fn from_phase(p: ImePhase) -> Self {
        match p {
            ImePhase::Start => SerImePhase::Start,
            ImePhase::Update => SerImePhase::Update,
            ImePhase::Commit => SerImePhase::Commit,
            ImePhase::Cancel => SerImePhase::Cancel,
        }
    }

    fn into_phase(self) -> ImePhase {
        match self {
            SerImePhase::Start => ImePhase::Start,
            SerImePhase::Update => ImePhase::Update,
            SerImePhase::Commit => ImePhase::Commit,
            SerImePhase::Cancel => ImePhase::Cancel,
        }
    }
}

impl SerClipboardSource {
    fn from_source(s: ClipboardSource) -> Self {
        match s {
            ClipboardSource::Osc52 => SerClipboardSource::Osc52,
            ClipboardSource::Unknown => SerClipboardSource::Unknown,
        }
    }

    fn into_source(self) -> ClipboardSource {
        match self {
            SerClipboardSource::Osc52 => ClipboardSource::Osc52,
            SerClipboardSource::Unknown => ClipboardSource::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// EventTraceWriter — writes events to gzip-compressed JSONL
// ---------------------------------------------------------------------------

/// Writes event trace records to a JSONL file (optionally gzip-compressed).
pub struct EventTraceWriter<W: Write> {
    writer: BufWriter<W>,
    event_count: u64,
    evidence_count: u64,
    first_ts_ns: Option<u64>,
    last_ts_ns: u64,
}

impl EventTraceWriter<std::fs::File> {
    /// Create a writer for an uncompressed JSONL file.
    pub fn plain(
        path: impl AsRef<Path>,
        session_name: &str,
        terminal_size: (u16, u16),
    ) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Self::from_writer(file, session_name, terminal_size, None)
    }
}

impl EventTraceWriter<flate2::write::GzEncoder<std::fs::File>> {
    /// Create a writer for a gzip-compressed JSONL file.
    pub fn gzip(
        path: impl AsRef<Path>,
        session_name: &str,
        terminal_size: (u16, u16),
    ) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        Self::from_writer(encoder, session_name, terminal_size, None)
    }

    /// Create a writer for a gzip-compressed JSONL file with a seed.
    pub fn gzip_with_seed(
        path: impl AsRef<Path>,
        session_name: &str,
        terminal_size: (u16, u16),
        seed: u64,
    ) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        Self::from_writer(encoder, session_name, terminal_size, Some(seed))
    }
}

impl<W: Write> EventTraceWriter<W> {
    /// Create a writer wrapping any `Write` implementation.
    pub fn from_writer(
        writer: W,
        session_name: &str,
        terminal_size: (u16, u16),
        seed: Option<u64>,
    ) -> io::Result<Self> {
        let mut w = BufWriter::new(writer);

        // Write header record.
        let header = TraceRecord::Header {
            schema_version: SCHEMA_VERSION.to_string(),
            session_name: session_name.to_string(),
            terminal_size,
            seed,
        };
        serde_json::to_writer(&mut w, &header).map_err(io::Error::other)?;
        w.write_all(b"\n")?;

        Ok(Self {
            writer: w,
            event_count: 0,
            evidence_count: 0,
            first_ts_ns: None,
            last_ts_ns: 0,
        })
    }

    /// Record an ftui [`Event`] at the given nanosecond timestamp.
    pub fn record(&mut self, event: &Event, ts_ns: u64) -> io::Result<()> {
        let record = TraceRecord::from_event(event, ts_ns);
        self.write_record(&record)
    }

    /// Record a frame timing snapshot.
    pub fn record_frame_time(&mut self, ts_ns: u64, render_us: Option<u64>) -> io::Result<()> {
        let record = TraceRecord::FrameTime { ts_ns, render_us };
        self.write_record(&record)
    }

    /// Record an RNG seed capture.
    pub fn record_rng_seed(&mut self, ts_ns: u64, seed: u64) -> io::Result<()> {
        let record = TraceRecord::RngSeed { ts_ns, seed };
        self.write_record(&record)
    }

    /// Record an evidence ledger entry from a Bayesian decision point (bd-3mjjt.4).
    pub fn record_evidence(&mut self, entry: &EvidenceEntry, ts_ns: u64) -> io::Result<()> {
        let record = TraceRecord::Evidence {
            ts_ns,
            entry: SerEvidenceEntry::from_entry(entry),
        };
        self.write_record(&record)
    }

    /// Write any trace record.
    pub fn write_record(&mut self, record: &TraceRecord) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, record).map_err(io::Error::other)?;
        self.writer.write_all(b"\n")?;

        if let Some(ts) = record.ts_ns() {
            if self.first_ts_ns.is_none() {
                self.first_ts_ns = Some(ts);
            }
            self.last_ts_ns = ts;
        }

        // Count records by type.
        match record {
            TraceRecord::Header { .. } | TraceRecord::Summary { .. } => {}
            TraceRecord::Evidence { .. } => {
                self.evidence_count += 1;
                self.event_count += 1;
            }
            _ => self.event_count += 1,
        }

        Ok(())
    }

    /// Get the number of event records written (excluding header/summary).
    #[inline]
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Get the number of evidence records written.
    #[inline]
    pub fn evidence_count(&self) -> u64 {
        self.evidence_count
    }

    /// Finish the trace: write summary and flush.
    ///
    /// Returns the underlying writer for further use.
    pub fn finish(mut self) -> io::Result<W> {
        let total_duration_ns = self
            .first_ts_ns
            .map(|first| self.last_ts_ns.saturating_sub(first))
            .unwrap_or(0);

        let total_evidence = if self.evidence_count > 0 {
            Some(self.evidence_count)
        } else {
            None
        };

        let summary = TraceRecord::Summary {
            total_events: self.event_count,
            total_duration_ns,
            total_evidence,
        };
        serde_json::to_writer(&mut self.writer, &summary).map_err(io::Error::other)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        self.writer
            .into_inner()
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// EventTraceReader — reads JSONL (gzip or plain)
// ---------------------------------------------------------------------------

/// Reads event trace records from a JSONL file.
///
/// Automatically detects gzip-compressed files by the gzip magic bytes
/// (0x1f, 0x8b).
pub struct EventTraceReader;

impl EventTraceReader {
    /// Open a trace file and parse all records.
    ///
    /// Detects gzip compression automatically.
    pub fn open(path: impl AsRef<Path>) -> io::Result<TraceFile> {
        let data = std::fs::read(path.as_ref())?;
        Self::from_bytes(&data)
    }

    /// Parse trace records from raw bytes.
    ///
    /// Detects gzip compression automatically by checking magic bytes.
    pub fn from_bytes(data: &[u8]) -> io::Result<TraceFile> {
        let decompressed: Vec<u8>;
        let text_data = if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
            // Gzip compressed
            use flate2::read::GzDecoder;
            let mut decoder = GzDecoder::new(data);
            decompressed = Vec::new();
            let mut buf = decompressed;
            io::Read::read_to_end(&mut decoder, &mut buf)?;
            buf
        } else {
            data.to_vec()
        };

        let reader = BufReader::new(text_data.as_slice());
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: TraceRecord = serde_json::from_str(&line).map_err(io::Error::other)?;
            records.push(record);
        }

        Ok(TraceFile { records })
    }
}

/// A parsed event trace file.
#[derive(Debug, Clone)]
pub struct TraceFile {
    records: Vec<TraceRecord>,
}

impl TraceFile {
    /// Get all records.
    #[inline]
    pub fn records(&self) -> &[TraceRecord] {
        &self.records
    }

    /// Get the header record, if present.
    #[must_use]
    pub fn header(&self) -> Option<&TraceRecord> {
        self.records
            .first()
            .filter(|r| matches!(r, TraceRecord::Header { .. }))
    }

    /// Get the summary record, if present.
    #[must_use]
    pub fn summary(&self) -> Option<&TraceRecord> {
        self.records
            .last()
            .filter(|r| matches!(r, TraceRecord::Summary { .. }))
    }

    /// Extract only the event records (no header/summary/metadata).
    pub fn event_records(&self) -> Vec<&TraceRecord> {
        self.records
            .iter()
            .filter(|r| !matches!(r, TraceRecord::Header { .. } | TraceRecord::Summary { .. }))
            .collect()
    }

    /// Extract events with timestamps, suitable for replay.
    pub fn events_with_timestamps(&self) -> Vec<(Event, u64)> {
        self.records
            .iter()
            .filter_map(|r| {
                let event = r.to_event()?;
                let ts = r.ts_ns()?;
                Some((event, ts))
            })
            .collect()
    }

    /// Get the session seed from the header, if present.
    #[must_use]
    pub fn seed(&self) -> Option<u64> {
        match self.header()? {
            TraceRecord::Header { seed, .. } => *seed,
            _ => None,
        }
    }

    /// Get the terminal size from the header, if present.
    #[must_use]
    pub fn terminal_size(&self) -> Option<(u16, u16)> {
        match self.header()? {
            TraceRecord::Header { terminal_size, .. } => Some(*terminal_size),
            _ => None,
        }
    }

    /// Get the total event count from the summary.
    #[must_use]
    pub fn total_events(&self) -> Option<u64> {
        match self.summary()? {
            TraceRecord::Summary { total_events, .. } => Some(*total_events),
            _ => None,
        }
    }

    /// Get the total evidence count from the summary.
    #[must_use]
    pub fn total_evidence(&self) -> Option<u64> {
        match self.summary()? {
            TraceRecord::Summary { total_evidence, .. } => *total_evidence,
            _ => None,
        }
    }

    /// Extract evidence entries with timestamps from the trace.
    pub fn evidence_entries(&self) -> Vec<(&SerEvidenceEntry, u64)> {
        self.records
            .iter()
            .filter_map(|r| match r {
                TraceRecord::Evidence { ts_ns, entry } => Some((entry, *ts_ns)),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// EventReplayer — feeds events in order with timing
// ---------------------------------------------------------------------------

/// Replays events from a trace with nanosecond timing.
///
/// Events are yielded in recorded order. The caller is responsible for
/// honoring the timing (sleeping between events) or ignoring it for
/// fast-forward replay.
pub struct EventReplayer {
    events: Vec<(Event, u64)>,
    position: usize,
}

impl EventReplayer {
    /// Create a replayer from a list of events with timestamps.
    #[must_use]
    pub fn new(events: Vec<(Event, u64)>) -> Self {
        Self {
            events,
            position: 0,
        }
    }

    /// Create a replayer from a [`TraceFile`].
    #[must_use]
    pub fn from_trace(trace: &TraceFile) -> Self {
        Self::new(trace.events_with_timestamps())
    }

    /// Get the next event with its timestamp.
    ///
    /// Returns `None` when all events have been consumed.
    pub fn next_event(&mut self) -> Option<(Event, u64)> {
        if self.position >= self.events.len() {
            return None;
        }
        let item = self.events[self.position].clone();
        self.position += 1;
        Some(item)
    }

    /// Peek at the next event without consuming it.
    #[must_use]
    pub fn peek(&self) -> Option<&(Event, u64)> {
        self.events.get(self.position)
    }

    /// Get the delay in nanoseconds until the next event.
    ///
    /// Returns `None` if no more events. Returns 0 for the first event.
    #[must_use]
    pub fn delay_to_next_ns(&self) -> Option<u64> {
        let next = self.events.get(self.position)?;
        if self.position == 0 {
            return Some(0);
        }
        let prev = &self.events[self.position - 1];
        Some(next.1.saturating_sub(prev.1))
    }

    /// Get all remaining events as a slice.
    #[must_use]
    pub fn remaining(&self) -> &[(Event, u64)] {
        &self.events[self.position..]
    }

    /// Check if replay is complete.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.position >= self.events.len()
    }

    /// Get current position (number of events consumed).
    #[inline]
    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    /// Get total number of events.
    #[inline]
    #[must_use]
    pub fn total(&self) -> usize {
        self.events.len()
    }

    /// Reset replay to the beginning.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Advance all events due before or at the given timestamp.
    ///
    /// Returns events whose `ts_ns <= until_ns`.
    pub fn advance_until(&mut self, until_ns: u64) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some((_, ts)) = self.peek() {
            if *ts > until_ns {
                break;
            }
            if let Some((event, _)) = self.next_event() {
                out.push(event);
            }
        }
        out
    }

    /// Drain all remaining events (fast-forward replay).
    pub fn drain_all(&mut self) -> Vec<Event> {
        let mut out = Vec::with_capacity(self.events.len() - self.position);
        while let Some((event, _)) = self.next_event() {
            out.push(event);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// EvidenceVerifier — validates determinism of Bayesian decisions (bd-3mjjt.4)
// ---------------------------------------------------------------------------

/// Describes a field-level mismatch between a recorded and replayed evidence entry.
#[derive(Debug, Clone)]
pub struct EvidenceMismatch {
    /// Index of the evidence entry (0-based, in recording order).
    pub index: usize,
    /// Which field mismatched.
    pub field: String,
    /// Recorded value (formatted for display).
    pub recorded: String,
    /// Replayed value (formatted for display).
    pub replayed: String,
}

/// Verifies that Bayesian evidence entries emitted during replay match those
/// captured during recording.
///
/// Validates that all decisions are deterministic given the same input sequence:
/// same `decision_id`, same `action`, same `log_posterior` within epsilon.
pub struct EvidenceVerifier {
    /// Tolerance for floating-point comparisons.
    epsilon: f64,
    /// Recorded evidence entries from the trace file.
    recorded: Vec<SerEvidenceEntry>,
    /// Mismatches found during verification.
    mismatches: Vec<EvidenceMismatch>,
    /// Number of entries verified so far.
    verified_count: usize,
}

impl EvidenceVerifier {
    /// Create a new verifier with the given floating-point tolerance.
    ///
    /// `epsilon` is used for comparing `log_posterior`, `loss_avoided`,
    /// `bayes_factor`, and confidence interval bounds.
    #[must_use]
    pub fn new(epsilon: f64) -> Self {
        Self {
            epsilon,
            recorded: Vec::new(),
            mismatches: Vec::new(),
            verified_count: 0,
        }
    }

    /// Create a verifier pre-loaded with evidence from a [`TraceFile`].
    #[must_use]
    pub fn from_trace(trace: &TraceFile, epsilon: f64) -> Self {
        let recorded = trace
            .evidence_entries()
            .into_iter()
            .map(|(e, _)| e.clone())
            .collect();
        Self {
            epsilon,
            recorded,
            mismatches: Vec::new(),
            verified_count: 0,
        }
    }

    /// Load recorded evidence from a slice of serializable entries.
    pub fn load_recorded(&mut self, entries: &[SerEvidenceEntry]) {
        self.recorded = entries.to_vec();
        self.verified_count = 0;
        self.mismatches.clear();
    }

    /// Verify a replayed [`EvidenceEntry`] against the next recorded entry.
    ///
    /// Returns `true` if the entry matches (or there is no corresponding
    /// recorded entry to compare against). Returns `false` if a mismatch
    /// is found.
    pub fn verify(&mut self, entry: &EvidenceEntry) -> bool {
        let idx = self.verified_count;
        self.verified_count += 1;

        if idx >= self.recorded.len() {
            // More replayed entries than recorded — not a mismatch per se,
            // but caller can check counts.
            return true;
        }

        let recorded = &self.recorded[idx];
        let replayed = SerEvidenceEntry::from_entry(entry);
        let mut ok = true;

        // decision_id
        if recorded.decision_id != replayed.decision_id {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "decision_id".to_string(),
                recorded: recorded.decision_id.to_string(),
                replayed: replayed.decision_id.to_string(),
            });
            ok = false;
        }

        // domain
        if recorded.domain != replayed.domain {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "domain".to_string(),
                recorded: format!("{:?}", recorded.domain),
                replayed: format!("{:?}", replayed.domain),
            });
            ok = false;
        }

        // action
        if recorded.action != replayed.action {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "action".to_string(),
                recorded: recorded.action.clone(),
                replayed: replayed.action.clone(),
            });
            ok = false;
        }

        // log_posterior (within epsilon)
        if (recorded.log_posterior - replayed.log_posterior).abs() > self.epsilon {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "log_posterior".to_string(),
                recorded: format!("{:.6}", recorded.log_posterior),
                replayed: format!("{:.6}", replayed.log_posterior),
            });
            ok = false;
        }

        // loss_avoided (within epsilon)
        if (recorded.loss_avoided - replayed.loss_avoided).abs() > self.epsilon {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "loss_avoided".to_string(),
                recorded: format!("{:.6}", recorded.loss_avoided),
                replayed: format!("{:.6}", replayed.loss_avoided),
            });
            ok = false;
        }

        // confidence_interval (within epsilon)
        if (recorded.confidence_interval.0 - replayed.confidence_interval.0).abs() > self.epsilon
            || (recorded.confidence_interval.1 - replayed.confidence_interval.1).abs()
                > self.epsilon
        {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "confidence_interval".to_string(),
                recorded: format!(
                    "({:.6}, {:.6})",
                    recorded.confidence_interval.0, recorded.confidence_interval.1
                ),
                replayed: format!(
                    "({:.6}, {:.6})",
                    replayed.confidence_interval.0, replayed.confidence_interval.1
                ),
            });
            ok = false;
        }

        // evidence terms
        if recorded.evidence.len() != replayed.evidence.len() {
            self.mismatches.push(EvidenceMismatch {
                index: idx,
                field: "evidence.len".to_string(),
                recorded: recorded.evidence.len().to_string(),
                replayed: replayed.evidence.len().to_string(),
            });
            ok = false;
        } else {
            for (ti, (rec_term, rep_term)) in recorded
                .evidence
                .iter()
                .zip(replayed.evidence.iter())
                .enumerate()
            {
                if rec_term.label != rep_term.label {
                    self.mismatches.push(EvidenceMismatch {
                        index: idx,
                        field: format!("evidence[{ti}].label"),
                        recorded: rec_term.label.clone(),
                        replayed: rep_term.label.clone(),
                    });
                    ok = false;
                }
                if (rec_term.bayes_factor - rep_term.bayes_factor).abs() > self.epsilon {
                    self.mismatches.push(EvidenceMismatch {
                        index: idx,
                        field: format!("evidence[{ti}].bayes_factor"),
                        recorded: format!("{:.6}", rec_term.bayes_factor),
                        replayed: format!("{:.6}", rep_term.bayes_factor),
                    });
                    ok = false;
                }
            }
        }

        ok
    }

    /// Whether all verified entries matched their recorded counterparts.
    #[must_use]
    pub fn is_deterministic(&self) -> bool {
        self.mismatches.is_empty() && self.verified_count == self.recorded.len()
    }

    /// Get all mismatches found during verification.
    #[must_use]
    pub fn mismatches(&self) -> &[EvidenceMismatch] {
        &self.mismatches
    }

    /// Number of entries verified so far.
    #[must_use]
    pub fn verified_count(&self) -> usize {
        self.verified_count
    }

    /// Number of recorded entries to verify against.
    #[must_use]
    pub fn expected_count(&self) -> usize {
        self.recorded.len()
    }

    /// Human-readable summary of the verification result.
    pub fn summary(&self) -> String {
        if self.mismatches.is_empty() {
            if self.verified_count == self.recorded.len() {
                format!(
                    "PASS: all {} evidence entries are deterministic",
                    self.verified_count
                )
            } else {
                format!(
                    "INCOMPLETE: verified {}/{} evidence entries (no mismatches so far)",
                    self.verified_count,
                    self.recorded.len()
                )
            }
        } else {
            format!(
                "FAIL: {} mismatches in {} verified entries (of {} recorded)",
                self.mismatches.len(),
                self.verified_count,
                self.recorded.len()
            )
        }
    }

    /// Detailed mismatch report.
    pub fn detail_report(&self) -> String {
        if self.mismatches.is_empty() {
            return self.summary();
        }
        let mut out = self.summary();
        out.push('\n');
        for m in &self.mismatches {
            out.push_str(&format!(
                "  [{}] {}: recorded={}, replayed={}\n",
                m.index, m.field, m.recorded, m.replayed
            ));
        }
        out
    }

    /// Reset the verifier for another replay pass.
    pub fn reset(&mut self) {
        self.mismatches.clear();
        self.verified_count = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_events() -> Vec<(Event, u64)> {
        vec![
            (Event::Key(KeyEvent::new(KeyCode::Char('a'))), 1_000_000),
            (
                Event::Mouse(MouseEvent::new(
                    MouseEventKind::Down(MouseButton::Left),
                    10,
                    5,
                )),
                2_000_000,
            ),
            (
                Event::Resize {
                    width: 120,
                    height: 40,
                },
                3_000_000,
            ),
            (
                Event::Paste(PasteEvent::bracketed("hello world")),
                4_000_000,
            ),
            (Event::Ime(ImeEvent::commit("漢字")), 5_000_000),
            (Event::Focus(true), 6_000_000),
            (
                Event::Clipboard(ClipboardEvent::new("clip data", ClipboardSource::Osc52)),
                7_000_000,
            ),
            (Event::Tick, 8_000_000),
        ]
    }

    #[test]
    fn round_trip_all_event_types() {
        for (event, ts) in sample_events() {
            let record = TraceRecord::from_event(&event, ts);
            let recovered = record.to_event().expect("should convert back to event");
            assert_eq!(event, recovered, "round-trip failed for {event:?}");
            assert_eq!(record.ts_ns(), Some(ts));
        }
    }

    #[test]
    fn header_and_summary_have_no_event() {
        let header = TraceRecord::Header {
            schema_version: SCHEMA_VERSION.to_string(),
            session_name: "test".to_string(),
            terminal_size: (80, 24),
            seed: None,
        };
        assert!(header.to_event().is_none());
        assert!(header.ts_ns().is_none());

        let summary = TraceRecord::Summary {
            total_events: 5,
            total_duration_ns: 1_000_000,
            total_evidence: None,
        };
        assert!(summary.to_event().is_none());
        assert!(summary.ts_ns().is_none());
    }

    #[test]
    fn frame_time_and_rng_seed_no_event() {
        let ft = TraceRecord::FrameTime {
            ts_ns: 100,
            render_us: Some(500),
        };
        assert!(ft.to_event().is_none());
        assert_eq!(ft.ts_ns(), Some(100));

        let rng = TraceRecord::RngSeed {
            ts_ns: 200,
            seed: 42,
        };
        assert!(rng.to_event().is_none());
        assert_eq!(rng.ts_ns(), Some(200));
    }

    #[test]
    fn json_round_trip_all_records() {
        for (event, ts) in sample_events() {
            let record = TraceRecord::from_event(&event, ts);
            let json = serde_json::to_string(&record).expect("serialize");
            let parsed: TraceRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(record, parsed, "JSON round-trip failed for {event:?}");
        }
    }

    #[test]
    fn write_and_read_plain_jsonl() {
        let mut buf = Vec::new();
        {
            let mut writer =
                EventTraceWriter::from_writer(&mut buf, "test_session", (80, 24), Some(42))
                    .expect("create writer");

            for (event, ts) in sample_events() {
                writer.record(&event, ts).expect("record event");
            }
            writer
                .record_frame_time(9_000_000, Some(1234))
                .expect("frame_time");
            writer.record_rng_seed(10_000_000, 99).expect("rng_seed");

            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read trace");

        // Header
        assert!(trace.header().is_some());
        assert_eq!(trace.terminal_size(), Some((80, 24)));
        assert_eq!(trace.seed(), Some(42));

        // Summary
        assert_eq!(trace.total_events(), Some(10)); // 8 events + frame_time + rng_seed

        // Events
        let events = trace.events_with_timestamps();
        assert_eq!(events.len(), 8); // Only actual Event variants

        // Verify round-trip
        let sample = sample_events();
        for (i, (event, ts)) in events.iter().enumerate() {
            assert_eq!(*event, sample[i].0, "event {i} mismatch");
            assert_eq!(*ts, sample[i].1, "timestamp {i} mismatch");
        }
    }

    #[test]
    fn write_and_read_gzip() {
        let dir = tempfile::Builder::new()
            .prefix("ftui-event-trace-")
            .disable_cleanup(true)
            .tempdir()
            .expect("tempdir");
        let path = dir.path().join("trace.jsonl.gz");

        {
            let mut writer =
                EventTraceWriter::gzip(&path, "gz_test", (100, 30)).expect("create gzip writer");

            writer
                .record(&Event::Key(KeyEvent::new(KeyCode::Enter)), 1_000)
                .expect("record");
            writer
                .record(
                    &Event::Resize {
                        width: 120,
                        height: 40,
                    },
                    2_000,
                )
                .expect("record");

            let encoder = writer.finish().expect("finish");
            encoder.finish().expect("flush gzip");
        }

        let trace = EventTraceReader::open(&path).expect("read gzip");
        assert_eq!(trace.terminal_size(), Some((100, 30)));

        let events = trace.events_with_timestamps();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, Event::Key(KeyEvent::new(KeyCode::Enter)));
        assert_eq!(events[0].1, 1_000);
    }

    #[test]
    fn replayer_basic_lifecycle() {
        let events = sample_events();
        let mut replayer = EventReplayer::new(events.clone());

        assert!(!replayer.is_done());
        assert_eq!(replayer.total(), 8);
        assert_eq!(replayer.position(), 0);

        // First event
        let (e, ts) = replayer.next_event().expect("first");
        assert_eq!(e, events[0].0);
        assert_eq!(ts, events[0].1);
        assert_eq!(replayer.position(), 1);

        // Peek
        let (pe, pts) = replayer.peek().expect("peek");
        assert_eq!(*pe, events[1].0);
        assert_eq!(*pts, events[1].1);
        assert_eq!(replayer.position(), 1); // peek doesn't advance

        // Drain remaining
        let rest = replayer.drain_all();
        assert_eq!(rest.len(), 7);
        assert!(replayer.is_done());
    }

    #[test]
    fn replayer_advance_until() {
        let events = sample_events();
        let mut replayer = EventReplayer::new(events);

        // Advance to 4_000_000 (includes events at 1M, 2M, 3M, 4M)
        let batch = replayer.advance_until(4_000_000);
        assert_eq!(batch.len(), 4);
        assert_eq!(replayer.position(), 4);

        // Advance to 6_000_000 (includes 5M, 6M)
        let batch2 = replayer.advance_until(6_000_000);
        assert_eq!(batch2.len(), 2);
    }

    #[test]
    fn replayer_reset() {
        let events = sample_events();
        let mut replayer = EventReplayer::new(events);

        replayer.drain_all();
        assert!(replayer.is_done());

        replayer.reset();
        assert!(!replayer.is_done());
        assert_eq!(replayer.position(), 0);
    }

    #[test]
    fn replayer_delay_to_next() {
        let events = vec![
            (Event::Key(KeyEvent::new(KeyCode::Char('a'))), 100),
            (Event::Key(KeyEvent::new(KeyCode::Char('b'))), 350),
            (Event::Key(KeyEvent::new(KeyCode::Char('c'))), 500),
        ];
        let mut replayer = EventReplayer::new(events);

        assert_eq!(replayer.delay_to_next_ns(), Some(0)); // first event
        replayer.next_event();
        assert_eq!(replayer.delay_to_next_ns(), Some(250)); // 350 - 100
        replayer.next_event();
        assert_eq!(replayer.delay_to_next_ns(), Some(150)); // 500 - 350
        replayer.next_event();
        assert_eq!(replayer.delay_to_next_ns(), None); // done
    }

    #[test]
    fn replayer_from_trace_file() {
        let mut buf = Vec::new();
        {
            let mut writer = EventTraceWriter::from_writer(&mut buf, "replay_test", (80, 24), None)
                .expect("create writer");
            writer
                .record(&Event::Key(KeyEvent::new(KeyCode::Char('x'))), 100)
                .expect("record");
            writer.record(&Event::Focus(false), 200).expect("record");
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        let mut replayer = EventReplayer::from_trace(&trace);

        assert_eq!(replayer.total(), 2);
        let (e, ts) = replayer.next_event().unwrap();
        assert_eq!(e, Event::Key(KeyEvent::new(KeyCode::Char('x'))));
        assert_eq!(ts, 100);
    }

    #[test]
    fn trace_file_event_records_excludes_header_summary() {
        let mut buf = Vec::new();
        {
            let mut writer = EventTraceWriter::from_writer(&mut buf, "filter_test", (80, 24), None)
                .expect("create writer");
            writer.record(&Event::Tick, 100).expect("record");
            writer
                .record_frame_time(200, Some(500))
                .expect("frame_time");
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        let all = trace.records();
        assert_eq!(all.len(), 4); // header + tick + frame_time + summary

        let event_recs = trace.event_records();
        assert_eq!(event_recs.len(), 2); // tick + frame_time
    }

    #[test]
    fn key_with_modifiers_round_trip() {
        let event = Event::Key(
            KeyEvent::new(KeyCode::Char('c')).with_modifiers(Modifiers::CTRL | Modifiers::SHIFT),
        );
        let record = TraceRecord::from_event(&event, 42);
        let json = serde_json::to_string(&record).unwrap();
        let parsed: TraceRecord = serde_json::from_str(&json).unwrap();
        let recovered = parsed.to_event().unwrap();
        assert_eq!(event, recovered);
    }

    #[test]
    fn mouse_with_modifiers_round_trip() {
        let event = Event::Mouse(
            MouseEvent::new(MouseEventKind::Drag(MouseButton::Right), 50, 25)
                .with_modifiers(Modifiers::ALT),
        );
        let record = TraceRecord::from_event(&event, 999);
        let json = serde_json::to_string(&record).unwrap();
        let parsed: TraceRecord = serde_json::from_str(&json).unwrap();
        let recovered = parsed.to_event().unwrap();
        assert_eq!(event, recovered);
    }

    #[test]
    fn all_key_codes_round_trip() {
        let codes = [
            KeyCode::Char('z'),
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::F(1),
            KeyCode::F(12),
            KeyCode::Null,
            KeyCode::MediaPlayPause,
            KeyCode::MediaStop,
            KeyCode::MediaNextTrack,
            KeyCode::MediaPrevTrack,
        ];
        for code in codes {
            let event = Event::Key(KeyEvent::new(code));
            let record = TraceRecord::from_event(&event, 0);
            let recovered = record.to_event().unwrap();
            assert_eq!(event, recovered, "failed for {code:?}");
        }
    }

    #[test]
    fn all_mouse_event_kinds_round_trip() {
        let kinds = [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Middle),
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ];
        for kind in kinds {
            let event = Event::Mouse(MouseEvent::new(kind, 0, 0));
            let record = TraceRecord::from_event(&event, 0);
            let recovered = record.to_event().unwrap();
            assert_eq!(event, recovered, "failed for {kind:?}");
        }
    }

    #[test]
    fn empty_trace_file() {
        let mut buf = Vec::new();
        {
            let writer = EventTraceWriter::from_writer(&mut buf, "empty", (80, 24), None)
                .expect("create writer");
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        assert_eq!(trace.total_events(), Some(0));
        assert_eq!(trace.events_with_timestamps().len(), 0);
    }

    #[test]
    fn writer_event_count() {
        let mut buf = Vec::new();
        let mut writer = EventTraceWriter::from_writer(&mut buf, "count", (80, 24), None)
            .expect("create writer");

        assert_eq!(writer.event_count(), 0);
        writer.record(&Event::Tick, 100).unwrap();
        assert_eq!(writer.event_count(), 1);
        writer.record_frame_time(200, None).unwrap();
        assert_eq!(writer.event_count(), 2);
        writer.record(&Event::Focus(true), 300).unwrap();
        assert_eq!(writer.event_count(), 3);
    }

    #[test]
    fn json_schema_version_in_header() {
        let mut buf = Vec::new();
        {
            let writer = EventTraceWriter::from_writer(&mut buf, "schema", (80, 24), None)
                .expect("create writer");
            writer.finish().expect("finish");
        }

        let text = String::from_utf8(buf).expect("valid utf8");
        let first_line = text.lines().next().expect("header line");
        let parsed: serde_json::Value = serde_json::from_str(first_line).expect("parse json");
        assert_eq!(parsed["schema_version"], "event-trace-v1");
        assert_eq!(parsed["event"], "trace_header");
    }

    // ── bd-3mjjt.4: Evidence Capture & Verification Tests ───────────────

    use crate::unified_evidence::{
        DecisionDomain, EvidenceEntry, EvidenceEntryBuilder, EvidenceTerm,
    };

    fn make_test_evidence(domain: DecisionDomain, action: &'static str, id: u64) -> EvidenceEntry {
        EvidenceEntry {
            decision_id: id,
            timestamp_ns: id * 1_000_000,
            domain,
            log_posterior: 1.386,
            top_evidence: [
                Some(EvidenceTerm::new("change_rate", 4.0)),
                Some(EvidenceTerm::new("dirty_ratio", 2.5)),
                None,
            ],
            action,
            loss_avoided: 0.15,
            confidence_interval: (0.72, 0.95),
        }
    }

    #[test]
    fn evidence_record_has_no_event() {
        let entry = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        let record = TraceRecord::Evidence {
            ts_ns: 1000,
            entry: SerEvidenceEntry::from_entry(&entry),
        };
        assert!(record.to_event().is_none());
        assert_eq!(record.ts_ns(), Some(1000));
    }

    #[test]
    fn evidence_json_round_trip() {
        let entry = make_test_evidence(DecisionDomain::FrameBudget, "hold", 42);
        let record = TraceRecord::Evidence {
            ts_ns: 5000,
            entry: SerEvidenceEntry::from_entry(&entry),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: TraceRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, parsed);
    }

    #[test]
    fn evidence_json_schema_fields() {
        let entry = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 7);
        let record = TraceRecord::Evidence {
            ts_ns: 3000,
            entry: SerEvidenceEntry::from_entry(&entry),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(parsed["event"], "evidence");
        assert_eq!(parsed["ts_ns"], 3000);
        assert_eq!(parsed["entry"]["domain"], "diff_strategy");
        assert_eq!(parsed["entry"]["action"], "dirty_rows");
        assert_eq!(parsed["entry"]["decision_id"], 7);
        assert!(parsed["entry"]["log_posterior"].as_f64().is_some());
        assert!(parsed["entry"]["evidence"].as_array().is_some());
    }

    #[test]
    fn write_and_read_evidence_in_trace() {
        let mut buf = Vec::new();
        {
            let mut writer =
                EventTraceWriter::from_writer(&mut buf, "evidence_test", (80, 24), None)
                    .expect("create writer");

            writer.record(&Event::Tick, 1_000).expect("tick");
            writer
                .record_evidence(
                    &make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0),
                    1_500,
                )
                .expect("evidence 0");
            writer.record(&Event::Tick, 2_000).expect("tick");
            writer
                .record_evidence(
                    &make_test_evidence(DecisionDomain::FrameBudget, "hold", 1),
                    2_500,
                )
                .expect("evidence 1");

            assert_eq!(writer.event_count(), 4);
            assert_eq!(writer.evidence_count(), 2);
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        assert_eq!(trace.total_events(), Some(4));
        assert_eq!(trace.total_evidence(), Some(2));

        let evidence = trace.evidence_entries();
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].0.action, "dirty_rows");
        assert_eq!(evidence[0].1, 1_500);
        assert_eq!(evidence[1].0.action, "hold");
        assert_eq!(evidence[1].1, 2_500);

        // Events still work normally.
        let events = trace.events_with_timestamps();
        assert_eq!(events.len(), 2); // Only Tick events convert to Event
    }

    #[test]
    fn trace_without_evidence_has_none_total() {
        let mut buf = Vec::new();
        {
            let mut writer = EventTraceWriter::from_writer(&mut buf, "no_ev", (80, 24), None)
                .expect("create writer");
            writer.record(&Event::Tick, 100).expect("tick");
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        assert!(trace.total_evidence().is_none());
        assert!(trace.evidence_entries().is_empty());
    }

    #[test]
    fn verifier_identical_entries_pass() {
        let entries: Vec<EvidenceEntry> = vec![
            make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0),
            make_test_evidence(DecisionDomain::FrameBudget, "hold", 1),
            make_test_evidence(DecisionDomain::VoiSampling, "sample", 2),
        ];

        // Record
        let recorded: Vec<SerEvidenceEntry> =
            entries.iter().map(SerEvidenceEntry::from_entry).collect();
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        // Replay with identical entries
        for entry in &entries {
            assert!(verifier.verify(entry));
        }
        assert!(verifier.is_deterministic());
        assert!(verifier.mismatches().is_empty());
        assert!(verifier.summary().contains("PASS"));
    }

    #[test]
    fn verifier_detects_action_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        // Replay with different action
        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "full", 0);
        replayed.decision_id = 0;
        assert!(!verifier.verify(&replayed));
        assert!(!verifier.is_deterministic());
        assert_eq!(verifier.mismatches().len(), 1);
        assert_eq!(verifier.mismatches()[0].field, "action");
    }

    #[test]
    fn verifier_detects_posterior_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        replayed.log_posterior = 2.0; // different
        assert!(!verifier.verify(&replayed));
        assert!(
            verifier
                .mismatches()
                .iter()
                .any(|m| m.field == "log_posterior")
        );
    }

    #[test]
    fn verifier_tolerance_allows_small_differences() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(0.01); // generous epsilon
        verifier.load_recorded(&recorded);

        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        replayed.log_posterior = 1.386 + 0.005; // within epsilon
        replayed.loss_avoided = 0.15 + 0.002; // within epsilon
        assert!(verifier.verify(&replayed));
        assert!(verifier.is_deterministic());
    }

    #[test]
    fn verifier_detects_domain_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let replayed = make_test_evidence(DecisionDomain::FrameBudget, "dirty_rows", 0);
        assert!(!verifier.verify(&replayed));
        assert!(verifier.mismatches().iter().any(|m| m.field == "domain"));
    }

    #[test]
    fn verifier_detects_evidence_term_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        // Change evidence term
        replayed.top_evidence[0] = Some(EvidenceTerm::new("different_signal", 4.0));
        assert!(!verifier.verify(&replayed));
        assert!(
            verifier
                .mismatches()
                .iter()
                .any(|m| m.field.contains("label"))
        );
    }

    #[test]
    fn verifier_detects_evidence_bayes_factor_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        replayed.top_evidence[0] = Some(EvidenceTerm::new("change_rate", 9.0)); // different BF
        assert!(!verifier.verify(&replayed));
        assert!(
            verifier
                .mismatches()
                .iter()
                .any(|m| m.field.contains("bayes_factor"))
        );
    }

    #[test]
    fn verifier_detects_confidence_interval_mismatch() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let mut replayed = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        replayed.confidence_interval = (0.50, 0.80); // different
        assert!(!verifier.verify(&replayed));
        assert!(
            verifier
                .mismatches()
                .iter()
                .any(|m| m.field == "confidence_interval")
        );
    }

    #[test]
    fn verifier_from_trace_integration() {
        let mut buf = Vec::new();
        let entries = vec![
            make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0),
            make_test_evidence(DecisionDomain::FrameBudget, "hold", 1),
        ];
        {
            let mut writer =
                EventTraceWriter::from_writer(&mut buf, "verifier_test", (80, 24), None)
                    .expect("create writer");
            for (i, entry) in entries.iter().enumerate() {
                writer
                    .record_evidence(entry, (i as u64 + 1) * 1000)
                    .expect("record evidence");
            }
            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");
        let mut verifier = EvidenceVerifier::from_trace(&trace, 1e-10);

        // Replay with identical entries
        for entry in &entries {
            assert!(verifier.verify(entry));
        }
        assert!(verifier.is_deterministic());
        assert_eq!(verifier.verified_count(), 2);
        assert_eq!(verifier.expected_count(), 2);
    }

    #[test]
    fn verifier_reset() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        // First pass — mismatch
        let replayed = make_test_evidence(DecisionDomain::DiffStrategy, "full", 0);
        verifier.verify(&replayed);
        assert!(!verifier.is_deterministic());

        // Reset and replay correctly
        verifier.reset();
        assert_eq!(verifier.verified_count(), 0);
        assert!(verifier.mismatches().is_empty());

        let correct = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        assert!(verifier.verify(&correct));
        assert!(verifier.is_deterministic());
    }

    #[test]
    fn verifier_incomplete_reports_correctly() {
        let recorded = vec![
            SerEvidenceEntry::from_entry(&make_test_evidence(
                DecisionDomain::DiffStrategy,
                "dirty_rows",
                0,
            )),
            SerEvidenceEntry::from_entry(&make_test_evidence(
                DecisionDomain::FrameBudget,
                "hold",
                1,
            )),
        ];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        // Only verify first entry
        let entry = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        verifier.verify(&entry);
        assert!(!verifier.is_deterministic()); // not all verified
        assert!(verifier.summary().contains("INCOMPLETE"));
    }

    #[test]
    fn verifier_detail_report_shows_fields() {
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let replayed = make_test_evidence(DecisionDomain::DiffStrategy, "full", 0);
        verifier.verify(&replayed);

        let report = verifier.detail_report();
        assert!(report.contains("FAIL"));
        assert!(report.contains("action"));
        assert!(report.contains("dirty_rows"));
        assert!(report.contains("full"));
    }

    #[test]
    fn verifier_extra_replayed_entries_accepted() {
        // If replay produces more evidence than recorded, extra entries
        // are accepted (no panic, no false mismatch).
        let recorded = vec![SerEvidenceEntry::from_entry(&make_test_evidence(
            DecisionDomain::DiffStrategy,
            "dirty_rows",
            0,
        ))];
        let mut verifier = EvidenceVerifier::new(1e-10);
        verifier.load_recorded(&recorded);

        let first = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0);
        let extra = make_test_evidence(DecisionDomain::FrameBudget, "hold", 1);
        assert!(verifier.verify(&first));
        assert!(verifier.verify(&extra)); // extra entry — no panic
        assert_eq!(verifier.verified_count(), 2);
    }

    #[test]
    fn ser_decision_domain_round_trip() {
        for domain in DecisionDomain::ALL {
            let ser = SerDecisionDomain::from_domain(domain);
            let back = ser.into_domain();
            assert_eq!(domain, back);

            // JSON round-trip
            let json = serde_json::to_string(&ser).expect("serialize");
            let parsed: SerDecisionDomain = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(ser, parsed);
        }
    }

    #[test]
    fn ser_evidence_entry_from_entry_preserves_fields() {
        let entry = EvidenceEntryBuilder::new(DecisionDomain::PaletteScoring, 42, 999_000)
            .log_posterior(2.0)
            .evidence("match_type", 9.0)
            .evidence("position", 1.5)
            .action("exact")
            .loss_avoided(0.8)
            .confidence_interval(0.90, 0.99)
            .build();

        let ser = SerEvidenceEntry::from_entry(&entry);
        assert_eq!(ser.decision_id, 42);
        assert_eq!(ser.domain, SerDecisionDomain::PaletteScoring);
        assert!((ser.log_posterior - 2.0).abs() < 1e-10);
        assert_eq!(ser.evidence.len(), 2);
        assert_eq!(ser.action, "exact");
        assert!((ser.loss_avoided - 0.8).abs() < 1e-10);
        assert!((ser.confidence_interval.0 - 0.90).abs() < 1e-10);
        assert!((ser.confidence_interval.1 - 0.99).abs() < 1e-10);
    }

    #[test]
    fn ser_evidence_entry_json_round_trip() {
        let entry = make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 5);
        let ser = SerEvidenceEntry::from_entry(&entry);
        let json = serde_json::to_string(&ser).expect("serialize");
        let parsed: SerEvidenceEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ser, parsed);
    }

    #[test]
    fn evidence_interleaved_with_events_in_trace() {
        // Verify that evidence and events can be interleaved and extracted independently.
        let mut buf = Vec::new();
        {
            let mut writer = EventTraceWriter::from_writer(&mut buf, "interleaved", (80, 24), None)
                .expect("create writer");

            writer
                .record(&Event::Key(KeyEvent::new(KeyCode::Char('a'))), 1_000)
                .expect("key");
            writer
                .record_evidence(
                    &make_test_evidence(DecisionDomain::DiffStrategy, "dirty_rows", 0),
                    1_500,
                )
                .expect("evidence");
            writer
                .record(&Event::Key(KeyEvent::new(KeyCode::Char('b'))), 2_000)
                .expect("key");
            writer
                .record_evidence(
                    &make_test_evidence(DecisionDomain::VoiSampling, "sample", 1),
                    2_500,
                )
                .expect("evidence");
            writer
                .record(&Event::Key(KeyEvent::new(KeyCode::Char('c'))), 3_000)
                .expect("key");

            writer.finish().expect("finish");
        }

        let trace = EventTraceReader::from_bytes(&buf).expect("read");

        // Events (only actual Event types)
        let events = trace.events_with_timestamps();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].0, Event::Key(KeyEvent::new(KeyCode::Char('a'))));
        assert_eq!(events[1].0, Event::Key(KeyEvent::new(KeyCode::Char('b'))));
        assert_eq!(events[2].0, Event::Key(KeyEvent::new(KeyCode::Char('c'))));

        // Evidence (separate extraction)
        let evidence = trace.evidence_entries();
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].0.action, "dirty_rows");
        assert_eq!(evidence[1].0.action, "sample");
    }
}
