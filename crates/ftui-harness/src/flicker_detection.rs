#![forbid(unsafe_code)]

//! Flicker/Tear Detection Harness for FrankenTUI.
//!
//! Detects visual artifacts (flicker/tearing) by analyzing ANSI output streams
//! for sync output gaps, partial clears, and other anomalies.
//!
//! # Key Concepts
//!
//! - **Sync Output Mode**: DEC private mode 2026 (`?2026h`/`?2026l`) brackets
//!   synchronized frame updates. Content outside these brackets may cause tearing.
//! - **Partial Clears**: ED (Erase Display) or EL (Erase Line) sequences mid-frame
//!   can cause visible flicker if not properly synchronized.
//! - **Frame Boundaries**: A frame begins with `?2026h` (begin sync) and ends with
//!   `?2026l` (end sync). Content between these markers should be atomic.
//!
//! # Detection Rules
//!
//! 1. **Sync Gap**: Output occurs outside synchronized mode brackets
//! 2. **Partial Clear**: ED/EL commands issued mid-frame (may show blank rows)
//! 3. **Incomplete Frame**: Frame started but never completed (crash/timeout)
//! 4. **Interleaved Writes**: Multiple frames overlap (race condition)
//!
//! # JSONL Logging Schema
//!
//! All events are logged as JSONL with stable schema:
//! ```json
//! {
//!   "run_id": "uuid",
//!   "timestamp_ns": 1234567890,
//!   "event_type": "sync_gap|partial_clear|incomplete_frame|...",
//!   "severity": "warning|error|info",
//!   "details": { ... event-specific fields ... },
//!   "context": { "frame_id": 0, "byte_offset": 0, "line": 0 }
//! }
//! ```

use std::fmt::Write as FmtWrite;
use std::io::Write;

// ============================================================================
// Core Types
// ============================================================================

/// Severity level for flicker events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Informational event (e.g., frame boundary).
    Info,
    /// Potential issue that may cause visible artifacts.
    Warning,
    /// Definite flicker/tear detected.
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Type of flicker/tear event detected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventType {
    /// Frame started with DEC ?2026h.
    FrameStart,
    /// Frame ended with DEC ?2026l.
    FrameEnd,
    /// Output occurred outside synchronized mode.
    SyncGap,
    /// Erase operation (ED/EL) detected mid-frame.
    PartialClear,
    /// Frame started but never completed.
    IncompleteFrame,
    /// Multiple frames overlapping.
    InterleavedWrites,
    /// Cursor moved without content update (suspicious).
    SuspiciousCursorMove,
    /// Analysis completed.
    AnalysisComplete,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameStart => write!(f, "frame_start"),
            Self::FrameEnd => write!(f, "frame_end"),
            Self::SyncGap => write!(f, "sync_gap"),
            Self::PartialClear => write!(f, "partial_clear"),
            Self::IncompleteFrame => write!(f, "incomplete_frame"),
            Self::InterleavedWrites => write!(f, "interleaved_writes"),
            Self::SuspiciousCursorMove => write!(f, "suspicious_cursor_move"),
            Self::AnalysisComplete => write!(f, "analysis_complete"),
        }
    }
}

/// Context where the event occurred.
#[derive(Debug, Clone, Default)]
pub struct EventContext {
    /// Current frame ID (0 if no frame active).
    pub frame_id: u64,
    /// Byte offset in the input stream.
    pub byte_offset: usize,
    /// Line number in the output (for debugging).
    pub line: usize,
    /// Column in the output.
    pub column: usize,
}

/// Additional details for specific event types.
#[derive(Debug, Clone, Default)]
pub struct EventDetails {
    /// Description of the event.
    pub message: String,
    /// Bytes that triggered the event.
    pub trigger_bytes: Option<Vec<u8>>,
    /// Number of bytes outside sync.
    pub bytes_outside_sync: Option<usize>,
    /// Clear command type (ED=0, EL=1).
    pub clear_type: Option<u8>,
    /// Clear mode (0=to end, 1=to start, 2=all).
    pub clear_mode: Option<u8>,
    /// Rows affected by the operation.
    pub affected_rows: Option<Vec<u16>>,
    /// Frame statistics (for AnalysisComplete).
    pub stats: Option<AnalysisStats>,
}

/// A detected flicker/tear event.
#[derive(Debug, Clone)]
pub struct FlickerEvent {
    /// Unique run identifier.
    pub run_id: String,
    /// Nanosecond timestamp.
    pub timestamp_ns: u64,
    /// Type of event.
    pub event_type: EventType,
    /// Severity level.
    pub severity: Severity,
    /// Event context.
    pub context: EventContext,
    /// Additional details.
    pub details: EventDetails,
}

impl FlickerEvent {
    /// Convert to JSONL format.
    pub fn to_jsonl(&self) -> String {
        let mut json = String::with_capacity(256);
        json.push('{');

        // Core fields
        write!(json, "\"run_id\":\"{}\",", self.run_id).unwrap();
        write!(json, "\"timestamp_ns\":{},", self.timestamp_ns).unwrap();
        write!(json, "\"event_type\":\"{}\",", self.event_type).unwrap();
        write!(json, "\"severity\":\"{}\",", self.severity).unwrap();

        // Context
        json.push_str("\"context\":{");
        write!(json, "\"frame_id\":{},", self.context.frame_id).unwrap();
        write!(json, "\"byte_offset\":{},", self.context.byte_offset).unwrap();
        write!(json, "\"line\":{},", self.context.line).unwrap();
        write!(json, "\"column\":{}", self.context.column).unwrap();
        json.push_str("},");

        // Details
        json.push_str("\"details\":{");
        write!(
            json,
            "\"message\":\"{}\"",
            escape_json(&self.details.message)
        )
        .unwrap();

        if let Some(ref bytes) = self.details.trigger_bytes {
            write!(
                json,
                ",\"trigger_bytes\":[{}]",
                bytes
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .unwrap();
        }
        if let Some(n) = self.details.bytes_outside_sync {
            write!(json, ",\"bytes_outside_sync\":{n}").unwrap();
        }
        if let Some(ct) = self.details.clear_type {
            write!(json, ",\"clear_type\":{ct}").unwrap();
        }
        if let Some(cm) = self.details.clear_mode {
            write!(json, ",\"clear_mode\":{cm}").unwrap();
        }
        if let Some(ref rows) = self.details.affected_rows {
            write!(
                json,
                ",\"affected_rows\":[{}]",
                rows.iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .unwrap();
        }
        if let Some(ref stats) = self.details.stats {
            write!(json, ",\"stats\":{{").unwrap();
            write!(json, "\"total_frames\":{},", stats.total_frames).unwrap();
            write!(json, "\"complete_frames\":{},", stats.complete_frames).unwrap();
            write!(json, "\"sync_gaps\":{},", stats.sync_gaps).unwrap();
            write!(json, "\"partial_clears\":{},", stats.partial_clears).unwrap();
            write!(json, "\"bytes_total\":{},", stats.bytes_total).unwrap();
            write!(json, "\"bytes_in_sync\":{},", stats.bytes_in_sync).unwrap();
            write!(json, "\"flicker_free\":{}", stats.is_flicker_free()).unwrap();
            json.push('}');
        }

        json.push_str("}}");
        json
    }
}

/// Escape a string for JSON output.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out
}

/// Statistics from analysis.
#[derive(Debug, Clone, Default)]
pub struct AnalysisStats {
    /// Total frames started.
    pub total_frames: u64,
    /// Frames that completed (had matching end).
    pub complete_frames: u64,
    /// Number of sync gap events.
    pub sync_gaps: u64,
    /// Number of partial clear events.
    pub partial_clears: u64,
    /// Total bytes processed.
    pub bytes_total: usize,
    /// Bytes within sync brackets.
    pub bytes_in_sync: usize,
}

impl AnalysisStats {
    /// Returns true if no flicker-inducing events were detected.
    pub fn is_flicker_free(&self) -> bool {
        self.sync_gaps == 0 && self.partial_clears == 0 && self.total_frames == self.complete_frames
    }

    /// Percentage of bytes within sync brackets.
    pub fn sync_coverage(&self) -> f64 {
        if self.bytes_total == 0 {
            100.0
        } else {
            (self.bytes_in_sync as f64 / self.bytes_total as f64) * 100.0
        }
    }
}

// ============================================================================
// Parser State
// ============================================================================

/// Parser state for ANSI sequence detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi,
    CsiParam,
    CsiPrivate,
}

/// Flicker detection analyzer.
pub struct FlickerDetector {
    /// Unique run ID for this analysis session.
    run_id: String,
    /// Current parser state.
    state: ParserState,
    /// CSI parameter accumulator.
    csi_params: Vec<u16>,
    /// Current CSI parameter being parsed.
    csi_current: u16,
    /// Whether we're in DEC private mode sequence.
    csi_private: bool,
    /// Whether synchronized output is active.
    sync_active: bool,
    /// Current frame ID.
    frame_id: u64,
    /// Byte offset in stream.
    byte_offset: usize,
    /// Current line.
    line: usize,
    /// Current column.
    column: usize,
    /// Bytes in current sync gap.
    gap_bytes: usize,
    /// Detected events.
    events: Vec<FlickerEvent>,
    /// Statistics.
    stats: AnalysisStats,
    /// Timestamp generator (monotonic counter for testing).
    timestamp_counter: u64,
}

impl FlickerDetector {
    /// Create a new detector with the given run ID.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            state: ParserState::Ground,
            csi_params: Vec::with_capacity(16),
            csi_current: 0,
            csi_private: false,
            sync_active: false,
            frame_id: 0,
            byte_offset: 0,
            line: 0,
            column: 0,
            gap_bytes: 0,
            events: Vec::new(),
            stats: AnalysisStats::default(),
            timestamp_counter: 0,
        }
    }

    /// Create a detector with a random UUID run ID.
    pub fn with_random_id() -> Self {
        let id = format!(
            "{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        Self::new(id)
    }

    /// Get the run ID.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Get collected events.
    pub fn events(&self) -> &[FlickerEvent] {
        &self.events
    }

    /// Get analysis statistics.
    pub fn stats(&self) -> &AnalysisStats {
        &self.stats
    }

    /// Check if the analyzed stream is flicker-free.
    pub fn is_flicker_free(&self) -> bool {
        self.stats.is_flicker_free()
    }

    /// Feed bytes to the detector.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.advance(byte);
            self.byte_offset += 1;
            self.stats.bytes_total += 1;
            if self.sync_active {
                self.stats.bytes_in_sync += 1;
            }
        }
    }

    /// Feed a string to the detector.
    pub fn feed_str(&mut self, s: &str) {
        self.feed(s.as_bytes());
    }

    /// Finalize analysis and generate summary event.
    pub fn finalize(&mut self) {
        // Check for incomplete frame
        if self.sync_active {
            self.emit_event(
                EventType::IncompleteFrame,
                Severity::Error,
                EventDetails {
                    message: format!("Frame {} never completed", self.frame_id),
                    ..Default::default()
                },
            );
            self.stats.total_frames += 1; // Count incomplete frame
        }

        // Report any trailing sync gap bytes that were never reported
        // (happens when stream ends without any sync frames)
        if self.gap_bytes > 0 {
            self.emit_event(
                EventType::SyncGap,
                Severity::Warning,
                EventDetails {
                    message: format!("{} bytes written outside sync mode", self.gap_bytes),
                    bytes_outside_sync: Some(self.gap_bytes),
                    ..Default::default()
                },
            );
            self.stats.sync_gaps += 1;
        }

        // Emit analysis complete event
        self.emit_event(
            EventType::AnalysisComplete,
            if self.stats.is_flicker_free() { Severity::Info } else { Severity::Warning },
            EventDetails {
                message: format!(
                    "Analysis complete: {} frames, {} sync gaps, {} partial clears, {:.1}% sync coverage",
                    self.stats.total_frames,
                    self.stats.sync_gaps,
                    self.stats.partial_clears,
                    self.stats.sync_coverage()
                ),
                stats: Some(self.stats.clone()),
                ..Default::default()
            },
        );
    }

    /// Write all events to a writer in JSONL format.
    pub fn write_jsonl<W: Write>(&self, mut writer: W) -> std::io::Result<()> {
        for event in &self.events {
            writeln!(writer, "{}", event.to_jsonl())?;
        }
        Ok(())
    }

    /// Get JSONL output as a string.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for event in &self.events {
            out.push_str(&event.to_jsonl());
            out.push('\n');
        }
        out
    }

    fn next_timestamp(&mut self) -> u64 {
        self.timestamp_counter += 1;
        self.timestamp_counter
    }

    fn emit_event(&mut self, event_type: EventType, severity: Severity, details: EventDetails) {
        let event = FlickerEvent {
            run_id: self.run_id.clone(),
            timestamp_ns: self.next_timestamp(),
            event_type,
            severity,
            context: EventContext {
                frame_id: self.frame_id,
                byte_offset: self.byte_offset,
                line: self.line,
                column: self.column,
            },
            details,
        };
        self.events.push(event);
    }

    fn advance(&mut self, byte: u8) {
        match self.state {
            ParserState::Ground => self.ground(byte),
            ParserState::Escape => self.escape(byte),
            ParserState::Csi | ParserState::CsiParam | ParserState::CsiPrivate => self.csi(byte),
        }

        // Track line/column
        if byte == b'\n' {
            self.line += 1;
            self.column = 0;
        } else if (0x20..0x7f).contains(&byte) {
            self.column += 1;
        }
    }

    fn ground(&mut self, byte: u8) {
        match byte {
            0x1b => {
                self.state = ParserState::Escape;
            }
            // Visible character output outside sync mode
            0x20..=0x7e if !self.sync_active => {
                self.gap_bytes += 1;
                // Only emit after accumulating some bytes to reduce noise
                if self.gap_bytes == 1 {
                    // First byte of gap - we'll report when sync starts or at finalize
                }
            }
            0x20..=0x7e => {
                // Normal output in sync mode - good
            }
            _ => {}
        }
    }

    fn escape(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.state = ParserState::Csi;
                self.csi_params.clear();
                self.csi_current = 0;
                self.csi_private = false;
            }
            _ => {
                self.state = ParserState::Ground;
            }
        }
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'?' => {
                self.csi_private = true;
                self.state = ParserState::CsiPrivate;
            }
            b'0'..=b'9' => {
                self.csi_current = self.csi_current.saturating_mul(10) + (byte - b'0') as u16;
                self.state = ParserState::CsiParam;
            }
            b';' => {
                self.csi_params.push(self.csi_current);
                self.csi_current = 0;
            }
            b'h' => {
                self.csi_params.push(self.csi_current);
                self.handle_set_mode();
                self.state = ParserState::Ground;
            }
            b'l' => {
                self.csi_params.push(self.csi_current);
                self.handle_reset_mode();
                self.state = ParserState::Ground;
            }
            b'J' => {
                // ED: Erase Display
                self.csi_params.push(self.csi_current);
                self.handle_erase_display();
                self.state = ParserState::Ground;
            }
            b'K' => {
                // EL: Erase Line
                self.csi_params.push(self.csi_current);
                self.handle_erase_line();
                self.state = ParserState::Ground;
            }
            b'H' | b'f' => {
                // CUP: Cursor Position
                self.csi_params.push(self.csi_current);
                // Cursor movement during frame is normal, but suspicious outside frame
                if !self.sync_active && self.gap_bytes > 0 {
                    // Cursor move with prior gap bytes suggests interleaved writes
                }
                self.state = ParserState::Ground;
            }
            b'm' | b'A'..=b'G' | b's' | b'u' => {
                // SGR, cursor movement, save/restore - normal operations
                self.state = ParserState::Ground;
            }
            _ if (0x40..=0x7e).contains(&byte) => {
                // Unknown CSI final byte
                self.state = ParserState::Ground;
            }
            _ => {
                // Continue parsing
            }
        }
    }

    fn handle_set_mode(&mut self) {
        if self.csi_private {
            // DEC private mode - check for sync-output (2026) first
            let has_sync = self.csi_params.contains(&2026);
            if has_sync {
                // Begin synchronized output
                self.handle_sync_begin();
            }
        }
    }

    fn handle_reset_mode(&mut self) {
        if self.csi_private {
            // Check for sync-output (2026) first
            let has_sync = self.csi_params.contains(&2026);
            if has_sync {
                // End synchronized output
                self.handle_sync_end();
            }
        }
    }

    fn handle_sync_begin(&mut self) {
        // Report accumulated gap if any
        if self.gap_bytes > 0 {
            self.emit_event(
                EventType::SyncGap,
                Severity::Warning,
                EventDetails {
                    message: format!("{} bytes written outside sync mode", self.gap_bytes),
                    bytes_outside_sync: Some(self.gap_bytes),
                    ..Default::default()
                },
            );
            self.stats.sync_gaps += 1;
        }

        self.sync_active = true;
        self.gap_bytes = 0;
        self.frame_id += 1;
        self.stats.total_frames += 1;

        self.emit_event(
            EventType::FrameStart,
            Severity::Info,
            EventDetails {
                message: format!("Frame {} started", self.frame_id),
                ..Default::default()
            },
        );
    }

    fn handle_sync_end(&mut self) {
        if !self.sync_active {
            // End without start - suspicious but not necessarily wrong
            return;
        }

        self.emit_event(
            EventType::FrameEnd,
            Severity::Info,
            EventDetails {
                message: format!("Frame {} completed", self.frame_id),
                ..Default::default()
            },
        );

        self.sync_active = false;
        self.stats.complete_frames += 1;
    }

    fn handle_erase_display(&mut self) {
        let mode = self.csi_params.first().copied().unwrap_or(0);

        // ED inside sync frame is fine; outside frame or partial clear is suspicious
        if self.sync_active && mode != 2 {
            // Partial erase during frame
            self.emit_event(
                EventType::PartialClear,
                Severity::Warning,
                EventDetails {
                    message: format!("Partial display erase (mode {}) during frame", mode),
                    clear_type: Some(0), // ED
                    clear_mode: Some(mode as u8),
                    ..Default::default()
                },
            );
            self.stats.partial_clears += 1;
        } else if !self.sync_active && mode == 2 {
            // Full clear outside sync - might be initialization, less suspicious
        }
    }

    fn handle_erase_line(&mut self) {
        let mode = self.csi_params.first().copied().unwrap_or(0);

        // Partial line erase during frame can cause flicker
        if self.sync_active && mode != 2 {
            self.emit_event(
                EventType::PartialClear,
                Severity::Warning,
                EventDetails {
                    message: format!("Partial line erase (mode {}) during frame", mode),
                    clear_type: Some(1), // EL
                    clear_mode: Some(mode as u8),
                    ..Default::default()
                },
            );
            self.stats.partial_clears += 1;
        }
    }
}

impl Default for FlickerDetector {
    fn default() -> Self {
        Self::new("default")
    }
}

// ============================================================================
// Assertion Helpers
// ============================================================================

/// Result of flicker detection analysis.
#[derive(Debug)]
pub struct FlickerAnalysis {
    /// Whether the stream is flicker-free.
    pub flicker_free: bool,
    /// Analysis statistics.
    pub stats: AnalysisStats,
    /// Detected events (errors and warnings only).
    pub issues: Vec<FlickerEvent>,
    /// Full JSONL log.
    pub jsonl: String,
}

impl FlickerAnalysis {
    /// Assert that the stream is flicker-free, panicking with details if not.
    pub fn assert_flicker_free(&self) {
        if !self.flicker_free {
            let mut msg = String::new();
            msg.push_str("\n=== Flicker Detection Failed ===\n\n");
            writeln!(msg, "Sync gaps: {}", self.stats.sync_gaps).unwrap();
            writeln!(msg, "Partial clears: {}", self.stats.partial_clears).unwrap();
            writeln!(
                msg,
                "Incomplete frames: {}",
                self.stats.total_frames - self.stats.complete_frames
            )
            .unwrap();
            writeln!(msg, "Sync coverage: {:.1}%", self.stats.sync_coverage()).unwrap();
            msg.push('\n');

            msg.push_str("Issues:\n");
            for issue in &self.issues {
                writeln!(
                    msg,
                    "  - [{}] {} at byte {}: {}",
                    issue.severity,
                    issue.event_type,
                    issue.context.byte_offset,
                    issue.details.message
                )
                .unwrap();
            }

            msg.push_str("\nFull JSONL log:\n");
            msg.push_str(&self.jsonl);

            assert!(self.flicker_free, "{msg}");
        }
    }
}

/// Analyze an ANSI byte stream for flicker/tearing.
pub fn analyze_stream(bytes: &[u8]) -> FlickerAnalysis {
    analyze_stream_with_id("analysis", bytes)
}

/// Analyze with a specific run ID.
pub fn analyze_stream_with_id(run_id: &str, bytes: &[u8]) -> FlickerAnalysis {
    let mut detector = FlickerDetector::new(run_id);
    detector.feed(bytes);
    detector.finalize();

    let issues: Vec<_> = detector
        .events()
        .iter()
        .filter(|e| matches!(e.severity, Severity::Warning | Severity::Error))
        .cloned()
        .collect();

    FlickerAnalysis {
        flicker_free: detector.is_flicker_free(),
        stats: detector.stats().clone(),
        issues,
        jsonl: detector.to_jsonl(),
    }
}

/// Analyze a string for flicker/tearing.
pub fn analyze_str(s: &str) -> FlickerAnalysis {
    analyze_stream(s.as_bytes())
}

/// Assert that an ANSI stream is flicker-free.
pub fn assert_flicker_free(bytes: &[u8]) {
    analyze_stream(bytes).assert_flicker_free();
}

/// Assert that an ANSI string is flicker-free.
pub fn assert_flicker_free_str(s: &str) {
    assert_flicker_free(s.as_bytes());
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::compute_text_checksum;

    // DEC private mode sequences
    const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
    const SYNC_END: &[u8] = b"\x1b[?2026l";

    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u32(&mut self) -> u32 {
            // Deterministic LCG (Numerical Recipes)
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 32) as u32
        }

        fn next_range(&mut self, max: usize) -> usize {
            if max == 0 {
                return 0;
            }
            (self.next_u32() as usize) % max
        }
    }

    fn make_synced_frame(content: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SYNC_BEGIN);
        out.extend_from_slice(content);
        out.extend_from_slice(SYNC_END);
        out
    }

    #[test]
    fn empty_stream_is_flicker_free() {
        let analysis = analyze_stream(b"");
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 0);
        assert_eq!(analysis.stats.sync_gaps, 0);
    }

    #[test]
    fn properly_synced_frame_is_flicker_free() {
        let frame = make_synced_frame(b"Hello, World!");
        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 1);
        assert_eq!(analysis.stats.complete_frames, 1);
        assert_eq!(analysis.stats.sync_gaps, 0);
    }

    #[test]
    fn multiple_synced_frames_are_flicker_free() {
        let mut stream = Vec::new();
        stream.extend(make_synced_frame(b"Frame 1"));
        stream.extend(make_synced_frame(b"Frame 2"));
        stream.extend(make_synced_frame(b"Frame 3"));

        let analysis = analyze_stream(&stream);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 3);
        assert_eq!(analysis.stats.complete_frames, 3);
    }

    #[test]
    fn output_without_sync_causes_gap() {
        let analysis = analyze_str("Hello without sync");
        // Text outside sync mode is a gap
        assert!(!analysis.flicker_free);
        assert!(analysis.stats.sync_gaps > 0);
    }

    #[test]
    fn sync_end_without_begin_is_ignored() {
        let analysis = analyze_stream(SYNC_END);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 0);
        assert_eq!(analysis.stats.complete_frames, 0);
        assert_eq!(analysis.stats.sync_gaps, 0);
    }

    #[test]
    fn output_before_sync_causes_gap() {
        let mut stream = b"Pre-sync content".to_vec();
        stream.extend(make_synced_frame(b"Synced content"));

        let analysis = analyze_stream(&stream);
        assert!(!analysis.flicker_free);
        assert_eq!(analysis.stats.sync_gaps, 1);
    }

    #[test]
    fn output_between_frames_causes_gap() {
        let mut stream = Vec::new();
        stream.extend(make_synced_frame(b"Frame 1"));
        stream.extend_from_slice(b"Gap content");
        stream.extend(make_synced_frame(b"Frame 2"));

        let analysis = analyze_stream(&stream);
        assert!(!analysis.flicker_free);
        assert_eq!(analysis.stats.sync_gaps, 1);
    }

    #[test]
    fn incomplete_frame_detected() {
        // Start sync but never end it
        let mut stream = Vec::new();
        stream.extend_from_slice(SYNC_BEGIN);
        stream.extend_from_slice(b"Content without end");

        let analysis = analyze_stream(&stream);
        assert!(!analysis.flicker_free);
        assert!(
            analysis
                .issues
                .iter()
                .any(|e| matches!(e.event_type, EventType::IncompleteFrame))
        );
    }

    #[test]
    fn partial_display_erase_detected() {
        // ED with mode 0 (erase to end) during frame
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[0J"); // Erase to end
        frame.extend_from_slice(b"Content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(!analysis.flicker_free);
        assert_eq!(analysis.stats.partial_clears, 1);
    }

    #[test]
    fn partial_line_erase_detected() {
        // EL with mode 0 (erase to end of line) during frame
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[0K"); // Erase to end of line
        frame.extend_from_slice(b"Content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(!analysis.flicker_free);
        assert_eq!(analysis.stats.partial_clears, 1);
    }

    #[test]
    fn full_display_clear_outside_sync_is_ok() {
        // ED 2 (clear all) outside sync is typical for initialization
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[2J"); // Clear screen
        stream.extend(make_synced_frame(b"First frame"));

        let analysis = analyze_stream(&stream);
        // Full clear before first frame is fine
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn full_line_clear_in_frame_is_ok() {
        // EL 2 (clear entire line) is okay - it's a complete operation
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[2K"); // Clear entire line
        frame.extend_from_slice(b"Content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn partial_erase_mode_one_detected_for_ed_and_el() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[1J"); // ED mode 1
        frame.extend_from_slice(b"\x1b[1K"); // EL mode 1
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 2);
        assert!(
            analysis
                .issues
                .iter()
                .any(|e| e.details.clear_mode == Some(1))
        );
    }

    #[test]
    fn jsonl_format_valid() {
        let frame = make_synced_frame(b"Test content");
        let mut detector = FlickerDetector::new("test-run");
        detector.feed(&frame);
        detector.finalize();

        let jsonl = detector.to_jsonl();
        assert!(!jsonl.is_empty());

        // Each line should be valid JSON (basic check)
        for line in jsonl.lines() {
            assert!(line.starts_with('{'));
            assert!(line.ends_with('}'));
            assert!(line.contains("\"run_id\":\"test-run\""));
            assert!(line.contains("\"event_type\":"));
            assert!(line.contains("\"severity\":"));
        }
    }

    #[test]
    fn jsonl_escapes_special_chars() {
        let event = FlickerEvent {
            run_id: "test".into(),
            timestamp_ns: 1,
            event_type: EventType::SyncGap,
            severity: Severity::Warning,
            context: EventContext::default(),
            details: EventDetails {
                message: "Contains \"quotes\" and \n newline".into(),
                ..Default::default()
            },
        };

        let json = event.to_jsonl();
        assert!(json.contains(r#"\\\"quotes\\\""#) || json.contains(r#"\"quotes\""#));
        assert!(json.contains("\\n"));
    }

    #[test]
    fn stats_sync_coverage_calculation() {
        let mut stats = AnalysisStats {
            bytes_total: 100,
            bytes_in_sync: 75,
            ..Default::default()
        };
        assert!((stats.sync_coverage() - 75.0).abs() < 0.01);

        stats.bytes_total = 0;
        assert!((stats.sync_coverage() - 100.0).abs() < 0.01);
    }

    #[test]
    fn detector_tracks_frame_ids() {
        let mut stream = Vec::new();
        stream.extend(make_synced_frame(b"1"));
        stream.extend(make_synced_frame(b"2"));
        stream.extend(make_synced_frame(b"3"));

        let mut detector = FlickerDetector::new("test");
        detector.feed(&stream);
        detector.finalize();

        let frame_starts: Vec<_> = detector
            .events()
            .iter()
            .filter(|e| matches!(e.event_type, EventType::FrameStart))
            .map(|e| e.context.frame_id)
            .collect();

        assert_eq!(frame_starts, vec![1, 2, 3]);
    }

    #[test]
    fn detector_tracks_byte_offsets() {
        let stream = make_synced_frame(b"Hello");
        let mut detector = FlickerDetector::new("test");
        detector.feed(&stream);
        detector.finalize();

        let last_event = detector.events().last().unwrap();
        assert_eq!(last_event.context.byte_offset, stream.len());
    }

    #[test]
    fn assert_flicker_free_passes_for_good_stream() {
        let frame = make_synced_frame(b"Good content");
        assert_flicker_free(&frame);
    }

    #[test]
    #[should_panic(expected = "Flicker Detection Failed")]
    fn assert_flicker_free_panics_for_bad_stream() {
        assert_flicker_free_str("Unsynced content");
    }

    #[test]
    fn complex_frame_with_styling() {
        // Realistic frame with cursor positioning and styling
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[H"); // Home
        frame.extend_from_slice(b"\x1b[2J"); // Clear (full, OK in sync)
        frame.extend_from_slice(b"\x1b[1;1H"); // Position
        frame.extend_from_slice(b"\x1b[1;31mRed\x1b[0m"); // Styled text
        frame.extend_from_slice(b"\x1b[2;1HLine 2");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        // Full clear inside sync is actually fine
        assert!(
            analysis.flicker_free,
            "Frame should be flicker-free: {:?}",
            analysis.issues
        );
    }

    #[test]
    fn realistic_render_loop_scenario() {
        let mut stream = Vec::new();

        // Simulate 10 frames of a render loop
        for i in 0..10 {
            stream.extend_from_slice(SYNC_BEGIN);
            stream.extend_from_slice(format!("\x1b[HFrame {i}").as_bytes());
            stream.extend_from_slice(b"\x1b[2;1HStatus: OK");
            stream.extend_from_slice(SYNC_END);
        }

        let analysis = analyze_stream(&stream);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 10);
        assert_eq!(analysis.stats.complete_frames, 10);
        // Coverage is ~80% because sync control sequences themselves aren't counted:
        // - SYNC_BEGIN: only the final 'h' byte is counted as in-sync (1/8)
        // - SYNC_END: all but the final 'l' byte (7/8) are in-sync
        assert!(
            analysis.stats.sync_coverage() > 75.0,
            "Expected >75% sync coverage, got {:.1}%",
            analysis.stats.sync_coverage()
        );
    }

    #[test]
    fn private_mode_with_extra_params_still_toggles_sync() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[?1;2026h");
        stream.extend_from_slice(b"payload");
        stream.extend_from_slice(b"\x1b[?1;2026l");

        let analysis = analyze_stream(&stream);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 1);
        assert_eq!(analysis.stats.complete_frames, 1);
    }

    #[test]
    fn write_jsonl_to_file() {
        let frame = make_synced_frame(b"Test");
        let mut detector = FlickerDetector::new("file-test");
        detector.feed(&frame);
        detector.finalize();

        let mut output = Vec::new();
        detector.write_jsonl(&mut output).unwrap();

        let jsonl = String::from_utf8(output).unwrap();
        assert!(jsonl.lines().count() > 0);
    }

    #[test]
    fn with_random_id_creates_unique_ids() {
        let d1 = FlickerDetector::with_random_id();
        let d2 = FlickerDetector::with_random_id();
        // Very unlikely to be equal given nanosecond precision
        assert_ne!(d1.run_id(), d2.run_id());
    }

    #[test]
    fn analysis_complete_severity_tracks_health() {
        let clean = analyze_stream(&make_synced_frame(b"ok"));
        let clean_last = clean
            .jsonl
            .lines()
            .last()
            .expect("analysis should emit at least one event");
        assert!(clean_last.contains("\"event_type\":\"analysis_complete\""));
        assert!(clean_last.contains("\"severity\":\"info\""));

        let noisy = analyze_stream(b"gap");
        let noisy_last = noisy
            .jsonl
            .lines()
            .last()
            .expect("analysis should emit at least one event");
        assert!(noisy_last.contains("\"event_type\":\"analysis_complete\""));
        assert!(noisy_last.contains("\"severity\":\"warning\""));
    }

    #[test]
    fn edge_case_empty_frame() {
        let frame = make_synced_frame(b"");
        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 1);
    }

    #[test]
    fn edge_case_nested_escapes() {
        // Malformed but shouldn't crash
        let mut stream = Vec::new();
        stream.extend_from_slice(SYNC_BEGIN);
        stream.extend_from_slice(b"\x1b\x1b\x1b[m"); // Weird escapes
        stream.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&stream);
        // Should complete without panic
        assert!(analysis.stats.total_frames >= 1);
    }

    #[test]
    fn property_synced_frames_are_flicker_free() {
        for seed in 0..8u64 {
            let mut rng = Lcg::new(seed);
            let mut stream = Vec::new();
            let frames = 5 + rng.next_range(8);
            for _ in 0..frames {
                let len = 8 + rng.next_range(32);
                let mut content = Vec::with_capacity(len);
                for _ in 0..len {
                    let byte = b'A' + (rng.next_range(26) as u8);
                    content.push(byte);
                }
                stream.extend(make_synced_frame(&content));
            }
            let analysis = analyze_stream(&stream);
            assert!(analysis.flicker_free, "seed {seed} should be flicker-free");
            assert_eq!(
                analysis.stats.total_frames, frames as u64,
                "seed {seed} should count all frames"
            );
        }
    }

    #[test]
    fn property_gap_detected_when_unsynced_bytes_present() {
        for seed in 0..8u64 {
            let mut rng = Lcg::new(seed ^ 0x5a5a5a5a);
            let mut stream = Vec::new();
            stream.extend(make_synced_frame(b"Frame 1"));
            let gap_len = 3 + rng.next_range(10);
            stream.extend(std::iter::repeat_n(b'Z', gap_len));
            stream.extend(make_synced_frame(b"Frame 2"));
            let analysis = analyze_stream(&stream);
            assert!(
                analysis.stats.sync_gaps > 0,
                "seed {seed} should detect sync gap"
            );
            assert!(
                !analysis.flicker_free,
                "seed {seed} should not be flicker-free"
            );
        }
    }

    #[test]
    fn golden_jsonl_checksum_fixture() {
        let stream = make_synced_frame(b"Flicker");
        let analysis = analyze_stream_with_id("golden", &stream);
        let checksum = compute_text_checksum(&analysis.jsonl);
        const EXPECTED: &str = "sha256:985ca693598f4559";
        assert_eq!(checksum, EXPECTED, "golden JSONL checksum drifted");
    }

    #[test]
    fn feed_str_matches_feed_bytes() {
        let stream = "\x1b[?2026hHello\x1b[?2026l";

        let mut from_str = FlickerDetector::new("from-str");
        from_str.feed_str(stream);
        from_str.finalize();

        let mut from_bytes = FlickerDetector::new("from-bytes");
        from_bytes.feed(stream.as_bytes());
        from_bytes.finalize();

        assert_eq!(
            from_str.stats().total_frames,
            from_bytes.stats().total_frames
        );
        assert_eq!(
            from_str.stats().complete_frames,
            from_bytes.stats().complete_frames
        );
        assert_eq!(from_str.stats().sync_gaps, from_bytes.stats().sync_gaps);
        assert_eq!(
            from_str.stats().partial_clears,
            from_bytes.stats().partial_clears
        );
    }

    // ================================================================
    // Edge-case tests (bd-1nz1c)
    // ================================================================

    // --- Severity ---

    #[test]
    fn severity_display_all_variants() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[test]
    fn severity_clone_copy_eq_hash() {
        let s = Severity::Warning;
        let s2 = s; // Copy
        assert_eq!(s, s2);
        let s3 = s;
        assert_eq!(s, s3);
        // Hash consistency
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Severity::Info);
        set.insert(Severity::Warning);
        set.insert(Severity::Error);
        assert_eq!(set.len(), 3);
        set.insert(Severity::Info); // duplicate
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn severity_debug() {
        let dbg = format!("{:?}", Severity::Error);
        assert!(dbg.contains("Error"));
    }

    // --- EventType ---

    #[test]
    fn event_type_display_all_variants() {
        assert_eq!(EventType::FrameStart.to_string(), "frame_start");
        assert_eq!(EventType::FrameEnd.to_string(), "frame_end");
        assert_eq!(EventType::SyncGap.to_string(), "sync_gap");
        assert_eq!(EventType::PartialClear.to_string(), "partial_clear");
        assert_eq!(EventType::IncompleteFrame.to_string(), "incomplete_frame");
        assert_eq!(
            EventType::InterleavedWrites.to_string(),
            "interleaved_writes"
        );
        assert_eq!(
            EventType::SuspiciousCursorMove.to_string(),
            "suspicious_cursor_move"
        );
        assert_eq!(EventType::AnalysisComplete.to_string(), "analysis_complete");
    }

    #[test]
    fn event_type_clone_eq_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(EventType::FrameStart.clone());
        set.insert(EventType::FrameEnd.clone());
        set.insert(EventType::SyncGap.clone());
        set.insert(EventType::PartialClear.clone());
        set.insert(EventType::IncompleteFrame.clone());
        set.insert(EventType::InterleavedWrites.clone());
        set.insert(EventType::SuspiciousCursorMove.clone());
        set.insert(EventType::AnalysisComplete.clone());
        assert_eq!(set.len(), 8);
    }

    #[test]
    fn event_type_debug() {
        let dbg = format!("{:?}", EventType::SyncGap);
        assert!(dbg.contains("SyncGap"));
    }

    // --- EventContext ---

    #[test]
    fn event_context_default_fields() {
        let ctx = EventContext::default();
        assert_eq!(ctx.frame_id, 0);
        assert_eq!(ctx.byte_offset, 0);
        assert_eq!(ctx.line, 0);
        assert_eq!(ctx.column, 0);
    }

    #[test]
    fn event_context_clone_debug() {
        let ctx = EventContext {
            frame_id: 42,
            byte_offset: 100,
            line: 5,
            column: 10,
        };
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.frame_id, 42);
        assert_eq!(ctx2.byte_offset, 100);
        let dbg = format!("{:?}", ctx);
        assert!(dbg.contains("42"));
    }

    // --- EventDetails ---

    #[test]
    fn event_details_default_fields() {
        let d = EventDetails::default();
        assert!(d.message.is_empty());
        assert!(d.trigger_bytes.is_none());
        assert!(d.bytes_outside_sync.is_none());
        assert!(d.clear_type.is_none());
        assert!(d.clear_mode.is_none());
        assert!(d.affected_rows.is_none());
        assert!(d.stats.is_none());
    }

    #[test]
    fn event_details_clone_debug() {
        let d = EventDetails {
            message: "test".into(),
            trigger_bytes: Some(vec![0x1b, 0x5b]),
            bytes_outside_sync: Some(10),
            clear_type: Some(0),
            clear_mode: Some(2),
            affected_rows: Some(vec![1, 2, 3]),
            stats: Some(AnalysisStats {
                total_frames: 5,
                complete_frames: 5,
                ..Default::default()
            }),
        };
        let d2 = d.clone();
        assert_eq!(d2.message, "test");
        assert_eq!(d2.trigger_bytes.as_ref().unwrap().len(), 2);
        assert_eq!(d2.affected_rows.as_ref().unwrap().len(), 3);
        let dbg = format!("{:?}", d);
        assert!(dbg.contains("test"));
    }

    // --- AnalysisStats ---

    #[test]
    fn analysis_stats_default() {
        let s = AnalysisStats::default();
        assert_eq!(s.total_frames, 0);
        assert_eq!(s.complete_frames, 0);
        assert_eq!(s.sync_gaps, 0);
        assert_eq!(s.partial_clears, 0);
        assert_eq!(s.bytes_total, 0);
        assert_eq!(s.bytes_in_sync, 0);
    }

    #[test]
    fn analysis_stats_is_flicker_free_combinations() {
        // All zeros → flicker-free
        assert!(AnalysisStats::default().is_flicker_free());

        // sync_gaps > 0 → not flicker-free
        assert!(
            !AnalysisStats {
                sync_gaps: 1,
                ..Default::default()
            }
            .is_flicker_free()
        );

        // partial_clears > 0 → not flicker-free
        assert!(
            !AnalysisStats {
                partial_clears: 1,
                ..Default::default()
            }
            .is_flicker_free()
        );

        // Incomplete frames → not flicker-free
        assert!(
            !AnalysisStats {
                total_frames: 3,
                complete_frames: 2,
                ..Default::default()
            }
            .is_flicker_free()
        );

        // All frames complete, no gaps or clears → flicker-free
        assert!(
            AnalysisStats {
                total_frames: 10,
                complete_frames: 10,
                bytes_total: 500,
                bytes_in_sync: 400,
                ..Default::default()
            }
            .is_flicker_free()
        );
    }

    #[test]
    fn analysis_stats_sync_coverage_partial() {
        let s = AnalysisStats {
            bytes_total: 200,
            bytes_in_sync: 50,
            ..Default::default()
        };
        assert!((s.sync_coverage() - 25.0).abs() < 0.01);
    }

    #[test]
    fn analysis_stats_sync_coverage_full() {
        let s = AnalysisStats {
            bytes_total: 100,
            bytes_in_sync: 100,
            ..Default::default()
        };
        assert!((s.sync_coverage() - 100.0).abs() < 0.01);
    }

    #[test]
    fn analysis_stats_clone_debug() {
        let s = AnalysisStats {
            total_frames: 7,
            complete_frames: 5,
            sync_gaps: 2,
            partial_clears: 1,
            bytes_total: 1000,
            bytes_in_sync: 800,
        };
        let s2 = s.clone();
        assert_eq!(s2.total_frames, 7);
        let dbg = format!("{:?}", s);
        assert!(dbg.contains("1000"));
    }

    // --- escape_json ---

    #[test]
    fn escape_json_empty() {
        assert_eq!(escape_json(""), "");
    }

    #[test]
    fn escape_json_no_special_chars() {
        assert_eq!(escape_json("hello world 123"), "hello world 123");
    }

    #[test]
    fn escape_json_quotes_and_backslash() {
        assert_eq!(escape_json(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_json(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn escape_json_newline_cr_tab() {
        assert_eq!(escape_json("a\nb"), "a\\nb");
        assert_eq!(escape_json("a\rb"), "a\\rb");
        assert_eq!(escape_json("a\tb"), "a\\tb");
    }

    #[test]
    fn escape_json_control_chars() {
        // NUL, BEL, BS
        let s = "\x00\x07\x08";
        let escaped = escape_json(s);
        assert!(escaped.contains("\\u0000"));
        assert!(escaped.contains("\\u0007"));
        assert!(escaped.contains("\\u0008"));
    }

    #[test]
    fn escape_json_unicode_passthrough() {
        assert_eq!(escape_json("日本語"), "日本語");
        assert_eq!(escape_json("emoji 🎉"), "emoji 🎉");
    }

    // --- FlickerEvent to_jsonl ---

    #[test]
    fn flicker_event_to_jsonl_all_optional_fields() {
        let event = FlickerEvent {
            run_id: "full".into(),
            timestamp_ns: 999,
            event_type: EventType::PartialClear,
            severity: Severity::Warning,
            context: EventContext {
                frame_id: 3,
                byte_offset: 42,
                line: 2,
                column: 5,
            },
            details: EventDetails {
                message: "test partial".into(),
                trigger_bytes: Some(vec![0x1b, 0x5b, 0x4a]),
                bytes_outside_sync: Some(17),
                clear_type: Some(0),
                clear_mode: Some(1),
                affected_rows: Some(vec![0, 1]),
                stats: Some(AnalysisStats {
                    total_frames: 10,
                    complete_frames: 9,
                    sync_gaps: 1,
                    partial_clears: 2,
                    bytes_total: 500,
                    bytes_in_sync: 450,
                }),
            },
        };

        let json = event.to_jsonl();
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
        assert!(json.contains("\"run_id\":\"full\""));
        assert!(json.contains("\"timestamp_ns\":999"));
        assert!(json.contains("\"event_type\":\"partial_clear\""));
        assert!(json.contains("\"severity\":\"warning\""));
        assert!(json.contains("\"frame_id\":3"));
        assert!(json.contains("\"byte_offset\":42"));
        assert!(json.contains("\"line\":2"));
        assert!(json.contains("\"column\":5"));
        assert!(json.contains("\"trigger_bytes\":[27,91,74]"));
        assert!(json.contains("\"bytes_outside_sync\":17"));
        assert!(json.contains("\"clear_type\":0"));
        assert!(json.contains("\"clear_mode\":1"));
        assert!(json.contains("\"affected_rows\":[0,1]"));
        assert!(json.contains("\"total_frames\":10"));
        assert!(json.contains("\"complete_frames\":9"));
        assert!(json.contains("\"flicker_free\":false"));
    }

    #[test]
    fn flicker_event_to_jsonl_minimal() {
        let event = FlickerEvent {
            run_id: "min".into(),
            timestamp_ns: 0,
            event_type: EventType::FrameStart,
            severity: Severity::Info,
            context: EventContext::default(),
            details: EventDetails::default(),
        };

        let json = event.to_jsonl();
        assert!(json.contains("\"run_id\":\"min\""));
        assert!(json.contains("\"message\":\"\""));
        // No optional fields
        assert!(!json.contains("trigger_bytes"));
        assert!(!json.contains("bytes_outside_sync"));
        assert!(!json.contains("clear_type"));
        assert!(!json.contains("affected_rows"));
        assert!(!json.contains("stats"));
    }

    #[test]
    fn flicker_event_clone_debug() {
        let event = FlickerEvent {
            run_id: "clone-test".into(),
            timestamp_ns: 42,
            event_type: EventType::SyncGap,
            severity: Severity::Warning,
            context: EventContext::default(),
            details: EventDetails::default(),
        };
        let e2 = event.clone();
        assert_eq!(e2.run_id, "clone-test");
        assert_eq!(e2.timestamp_ns, 42);
        let dbg = format!("{:?}", event);
        assert!(dbg.contains("clone-test"));
    }

    // --- FlickerDetector ---

    #[test]
    fn detector_default_impl() {
        let d = FlickerDetector::default();
        assert_eq!(d.run_id(), "default");
        assert!(d.events().is_empty());
        assert!(d.is_flicker_free());
    }

    #[test]
    fn detector_run_id_accessor() {
        let d = FlickerDetector::new("my-run-123");
        assert_eq!(d.run_id(), "my-run-123");
    }

    #[test]
    fn detector_stats_accessor() {
        let d = FlickerDetector::new("test");
        let stats = d.stats();
        assert_eq!(stats.total_frames, 0);
        assert_eq!(stats.bytes_total, 0);
    }

    #[test]
    fn detector_feed_str_independently() {
        let sync_begin = "\x1b[?2026h";
        let sync_end = "\x1b[?2026l";
        let mut d = FlickerDetector::new("str-test");
        d.feed_str(sync_begin);
        d.feed_str("Content");
        d.feed_str(sync_end);
        d.finalize();
        assert!(d.is_flicker_free());
        assert_eq!(d.stats().total_frames, 1);
        assert_eq!(d.stats().complete_frames, 1);
    }

    #[test]
    fn detector_incremental_feed() {
        // Feed frame one byte at a time
        let frame = make_synced_frame(b"Hello");
        let mut d = FlickerDetector::new("incr");
        for &byte in &frame {
            d.feed(&[byte]);
        }
        d.finalize();
        assert!(d.is_flicker_free());
        assert_eq!(d.stats().total_frames, 1);
    }

    #[test]
    fn detector_bytes_tracking() {
        let frame = make_synced_frame(b"AB");
        let mut d = FlickerDetector::new("bytes");
        d.feed(&frame);
        d.finalize();
        // SYNC_BEGIN=8 + "AB"=2 + SYNC_END=8 = 18 bytes total
        assert_eq!(d.stats().bytes_total, 18);
        // bytes_in_sync: counted while sync_active=true
        assert!(d.stats().bytes_in_sync > 0);
        assert!(d.stats().bytes_in_sync < d.stats().bytes_total);
    }

    #[test]
    fn detector_finalize_emits_analysis_complete() {
        let mut d = FlickerDetector::new("fin");
        d.finalize();
        let last = d.events().last().unwrap();
        assert!(matches!(last.event_type, EventType::AnalysisComplete));
        assert!(matches!(last.severity, Severity::Info)); // flicker-free
    }

    #[test]
    fn detector_finalize_incomplete_frame_severity() {
        let mut d = FlickerDetector::new("inc");
        d.feed(SYNC_BEGIN);
        d.feed(b"dangling content");
        d.finalize();
        let incomplete: Vec<_> = d
            .events()
            .iter()
            .filter(|e| matches!(e.event_type, EventType::IncompleteFrame))
            .collect();
        assert_eq!(incomplete.len(), 1);
        assert!(matches!(incomplete[0].severity, Severity::Error));
        let complete_evt = d.events().last().unwrap();
        assert!(matches!(
            complete_evt.event_type,
            EventType::AnalysisComplete
        ));
        assert!(matches!(complete_evt.severity, Severity::Warning));
    }

    #[test]
    fn detector_sync_end_without_start() {
        let mut d = FlickerDetector::new("no-start");
        d.feed(SYNC_END);
        d.finalize();
        assert_eq!(d.stats().total_frames, 0);
        assert_eq!(d.stats().complete_frames, 0);
    }

    #[test]
    fn detector_multiple_partial_clears() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[0J"); // Partial ED to-end
        frame.extend_from_slice(b"\x1b[1J"); // Partial ED to-start
        frame.extend_from_slice(b"\x1b[0K"); // Partial EL to-end
        frame.extend_from_slice(b"\x1b[1K"); // Partial EL to-start
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 4);
    }

    #[test]
    fn detector_ed_mode2_inside_sync_no_partial_clear() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[2J");
        frame.extend_from_slice(b"Content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn detector_el_mode2_inside_sync_no_partial_clear() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[2K");
        frame.extend_from_slice(b"Content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn detector_ed_mode1_partial_clear() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[1J");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(analysis.stats.partial_clears, 1);
    }

    #[test]
    fn detector_el_outside_sync_not_partial_clear() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[0K");
        stream.extend(make_synced_frame(b"Ok"));

        let analysis = analyze_stream(&stream);
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn detector_ed_outside_sync_not_partial_clear() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[0J");
        stream.extend(make_synced_frame(b"Ok"));

        let analysis = analyze_stream(&stream);
        assert_eq!(analysis.stats.partial_clears, 0);
    }

    #[test]
    fn detector_line_column_tracking() {
        let mut d = FlickerDetector::new("lc");
        d.feed(b"AB\nCD\nEF");
        d.finalize();
        let last = d.events().last().unwrap();
        assert_eq!(last.context.line, 2);
        assert_eq!(last.context.column, 2);
    }

    #[test]
    fn detector_only_visible_chars_are_gap_bytes() {
        let mut d = FlickerDetector::new("gap");
        d.feed(b"\x00\x01\x02\x03");
        d.finalize();
        assert!(d.is_flicker_free());
    }

    #[test]
    fn detector_gap_bytes_accumulated_across_regions() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"ABC");
        stream.extend(make_synced_frame(b"F1"));
        stream.extend_from_slice(b"DE");
        stream.extend(make_synced_frame(b"F2"));

        let analysis = analyze_stream(&stream);
        assert_eq!(analysis.stats.sync_gaps, 2);
    }

    #[test]
    fn detector_timestamp_monotonic() {
        let frame = make_synced_frame(b"Hi");
        let mut d = FlickerDetector::new("ts");
        d.feed(&frame);
        d.finalize();
        let timestamps: Vec<u64> = d.events().iter().map(|e| e.timestamp_ns).collect();
        for window in timestamps.windows(2) {
            assert!(
                window[1] > window[0],
                "Timestamps not monotonic: {:?}",
                timestamps
            );
        }
    }

    #[test]
    fn detector_write_jsonl_empty() {
        let d = FlickerDetector::new("empty");
        let mut output = Vec::new();
        d.write_jsonl(&mut output).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn detector_to_jsonl_empty() {
        let d = FlickerDetector::new("empty");
        assert!(d.to_jsonl().is_empty());
    }

    // --- Convenience functions ---

    #[test]
    fn analyze_str_convenience() {
        let sync_begin = "\x1b[?2026h";
        let sync_end = "\x1b[?2026l";
        let input = format!("{sync_begin}Hello{sync_end}");
        let analysis = analyze_str(&input);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn analyze_stream_with_id_custom_id() {
        let frame = make_synced_frame(b"Test");
        let analysis = analyze_stream_with_id("custom-42", &frame);
        assert!(analysis.flicker_free);
        assert!(analysis.jsonl.contains("custom-42"));
    }

    #[test]
    fn analyze_stream_default_id() {
        let frame = make_synced_frame(b"T");
        let analysis = analyze_stream(&frame);
        assert!(analysis.jsonl.contains("\"run_id\":\"analysis\""));
    }

    // --- FlickerAnalysis ---

    #[test]
    fn flicker_analysis_debug() {
        let analysis = analyze_stream(b"");
        let dbg = format!("{:?}", analysis);
        assert!(dbg.contains("flicker_free"));
        assert!(dbg.contains("stats"));
    }

    #[test]
    fn flicker_analysis_issues_only_warnings_and_errors() {
        let mut stream = Vec::new();
        stream.extend(make_synced_frame(b"Frame"));
        stream.extend_from_slice(b"Gap");
        stream.extend(make_synced_frame(b"Frame2"));

        let analysis = analyze_stream(&stream);
        for issue in &analysis.issues {
            assert!(
                matches!(issue.severity, Severity::Warning | Severity::Error),
                "Issue should be Warning or Error, got {:?}",
                issue.severity
            );
        }
        assert!(!analysis.issues.is_empty());
    }

    // --- CSI parsing ---

    #[test]
    fn csi_with_semicolons_multi_param() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[1;31m");
        frame.extend_from_slice(b"Red");
        frame.extend_from_slice(b"\x1b[0m");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn csi_cursor_movement_in_sync() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[5A"); // Up 5
        frame.extend_from_slice(b"\x1b[3B"); // Down 3
        frame.extend_from_slice(b"\x1b[10C"); // Right 10
        frame.extend_from_slice(b"\x1b[2D"); // Left 2
        frame.extend_from_slice(b"\x1b[s"); // Save
        frame.extend_from_slice(b"\x1b[u"); // Restore
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn csi_cursor_position_with_params() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[10;20H");
        frame.extend_from_slice(b"At position");
        frame.extend_from_slice(b"\x1b[5;15f");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn csi_unknown_final_byte() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[42z");
        frame.extend_from_slice(b"\x1b[0~");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn csi_dec_private_non_sync_mode() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[?25l"); // Hide cursor
        stream.extend(make_synced_frame(b"Content"));
        stream.extend_from_slice(b"\x1b[?25h"); // Show cursor

        let analysis = analyze_stream(&stream);
        assert!(analysis.flicker_free);
    }

    // --- Edge cases ---

    #[test]
    fn only_escape_sequences_no_content() {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1;1H");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
    }

    #[test]
    fn very_long_frame_content() {
        let content: Vec<u8> = (0..10_000).map(|i| b'A' + (i % 26) as u8).collect();
        let frame = make_synced_frame(&content);
        let analysis = analyze_stream(&frame);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 1);
    }

    #[test]
    fn many_small_frames() {
        let mut stream = Vec::new();
        for _ in 0..100 {
            stream.extend(make_synced_frame(b"X"));
        }
        let analysis = analyze_stream(&stream);
        assert!(analysis.flicker_free);
        assert_eq!(analysis.stats.total_frames, 100);
        assert_eq!(analysis.stats.complete_frames, 100);
    }

    #[test]
    fn escape_at_end_of_stream() {
        let mut d = FlickerDetector::new("esc-end");
        d.feed(b"\x1b");
        d.finalize();
        assert_eq!(d.stats().total_frames, 0);
    }

    #[test]
    fn csi_at_end_of_stream() {
        let mut d = FlickerDetector::new("csi-end");
        d.feed(b"\x1b[42");
        d.finalize();
        assert_eq!(d.stats().total_frames, 0);
    }

    #[test]
    fn csi_private_at_end_of_stream() {
        let mut d = FlickerDetector::new("dec-end");
        d.feed(b"\x1b[?2026");
        d.finalize();
        assert_eq!(d.stats().total_frames, 0);
    }

    #[test]
    fn multiple_gap_regions_correct_count() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"Gap1");
        stream.extend(make_synced_frame(b"F1"));
        stream.extend_from_slice(b"Gap2");
        stream.extend(make_synced_frame(b"F2"));
        stream.extend_from_slice(b"Gap3");

        let analysis = analyze_stream(&stream);
        assert_eq!(analysis.stats.sync_gaps, 3);
    }

    #[test]
    fn flicker_event_to_jsonl_escaped_message() {
        let event = FlickerEvent {
            run_id: "esc".into(),
            timestamp_ns: 1,
            event_type: EventType::SyncGap,
            severity: Severity::Warning,
            context: EventContext::default(),
            details: EventDetails {
                message: "has \"quotes\" and\nnewlines".into(),
                ..Default::default()
            },
        };
        let json = event.to_jsonl();
        assert!(json.contains("\\\"quotes\\\""));
        assert!(json.contains("\\n"));
    }

    #[test]
    fn flicker_event_to_jsonl_stats_flicker_free_true() {
        let event = FlickerEvent {
            run_id: "ok".into(),
            timestamp_ns: 1,
            event_type: EventType::AnalysisComplete,
            severity: Severity::Info,
            context: EventContext::default(),
            details: EventDetails {
                message: "done".into(),
                stats: Some(AnalysisStats {
                    total_frames: 5,
                    complete_frames: 5,
                    sync_gaps: 0,
                    partial_clears: 0,
                    bytes_total: 100,
                    bytes_in_sync: 80,
                }),
                ..Default::default()
            },
        };
        let json = event.to_jsonl();
        assert!(json.contains("\"flicker_free\":true"));
    }
}
