#![forbid(unsafe_code)]

//! Asciicast v2 session recording for FrankenTUI.
//!
//! Records terminal output in [asciicast v2 format](https://docs.asciinema.org/manual/asciicast/v2/),
//! enabling playback with asciinema-player or asciinema.org.
//!
//! # Quick Start
//!
//! ```
//! use ftui_harness::asciicast::{AsciicastRecorder, RecordConfig};
//! use std::io::Cursor;
//!
//! let mut output = Cursor::new(Vec::new());
//! let config = RecordConfig::new(80, 24);
//! let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();
//!
//! recorder.record_output(b"Hello, world!").unwrap();
//! recorder.record_output(b"\x1b[31mRed text\x1b[0m").unwrap();
//! recorder.finish().unwrap();
//!
//! let data = output.into_inner();
//! assert!(data.starts_with(b"{\"version\":2"));
//! ```
//!
//! # Input Recording (Optional)
//!
//! ```
//! use ftui_harness::asciicast::{AsciicastRecorder, RecordConfig};
//! use std::io::Cursor;
//!
//! let mut output = Cursor::new(Vec::new());
//! let config = RecordConfig::new(80, 24).with_input_recording(true);
//! let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();
//!
//! recorder.record_input(b"ls -la\n").unwrap();
//! recorder.record_output(b"file1.txt  file2.txt\n").unwrap();
//! recorder.finish().unwrap();
//! ```
//!
//! # File Recording
//!
//! ```no_run
//! use ftui_harness::asciicast::{AsciicastRecorder, RecordConfig};
//! use std::fs::File;
//! use std::io::BufWriter;
//!
//! let file = File::create("session.cast").unwrap();
//! let writer = BufWriter::new(file);
//! let config = RecordConfig::new(120, 40)
//!     .with_title("Demo Session")
//!     .with_env_shell("/bin/bash");
//!
//! let mut recorder = AsciicastRecorder::new(writer, config).unwrap();
//! // ... record events ...
//! recorder.finish().unwrap();
//! ```

use std::io::{self, Write};
use std::time::{Duration, Instant};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for asciicast recording.
#[derive(Debug, Clone)]
pub struct RecordConfig {
    /// Terminal width in columns.
    pub width: u16,
    /// Terminal height in rows.
    pub height: u16,
    /// Optional session title.
    pub title: Option<String>,
    /// Optional shell for env metadata.
    pub env_shell: Option<String>,
    /// Optional terminal type for env metadata.
    pub env_term: Option<String>,
    /// Record input events in addition to output.
    pub record_input: bool,
    /// Idle time limit in seconds (events beyond this are compressed).
    pub idle_time_limit: Option<f64>,
}

impl RecordConfig {
    /// Create a new configuration with required terminal dimensions.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            title: None,
            env_shell: None,
            env_term: None,
            record_input: false,
            idle_time_limit: None,
        }
    }

    /// Set the session title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the shell for env metadata.
    #[must_use]
    pub fn with_env_shell(mut self, shell: impl Into<String>) -> Self {
        self.env_shell = Some(shell.into());
        self
    }

    /// Set the terminal type for env metadata.
    #[must_use]
    pub fn with_env_term(mut self, term: impl Into<String>) -> Self {
        self.env_term = Some(term.into());
        self
    }

    /// Enable or disable input recording.
    #[must_use]
    pub fn with_input_recording(mut self, enabled: bool) -> Self {
        self.record_input = enabled;
        self
    }

    /// Set idle time limit (events beyond this duration are compressed).
    #[must_use]
    pub fn with_idle_time_limit(mut self, limit: f64) -> Self {
        self.idle_time_limit = Some(limit);
        self
    }
}

// ============================================================================
// Recorder
// ============================================================================

/// Asciicast v2 format recorder.
///
/// Records terminal output (and optionally input) events in NDJSON format
/// compatible with asciinema-player.
pub struct AsciicastRecorder<W: Write> {
    output: W,
    start_time: Instant,
    config: RecordConfig,
    event_count: u64,
    last_time: f64,
}

impl<W: Write> AsciicastRecorder<W> {
    /// Create a new recorder with the given output writer and configuration.
    ///
    /// Writes the asciicast v2 header immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the header fails.
    pub fn new(mut output: W, config: RecordConfig) -> io::Result<Self> {
        // Write header
        let header = Self::build_header(&config);
        writeln!(output, "{header}")?;

        Ok(Self {
            output,
            start_time: Instant::now(),
            config,
            event_count: 0,
            last_time: 0.0,
        })
    }

    /// Build the JSON header string.
    fn build_header(config: &RecordConfig) -> String {
        let mut header = String::with_capacity(256);
        header.push_str("{\"version\":2");
        header.push_str(&format!(",\"width\":{}", config.width));
        header.push_str(&format!(",\"height\":{}", config.height));

        // Timestamp (seconds since Unix epoch)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        header.push_str(&format!(",\"timestamp\":{timestamp}"));

        if let Some(ref title) = config.title {
            header.push_str(&format!(",\"title\":\"{}\"", escape_json_string(title)));
        }

        if let Some(limit) = config.idle_time_limit {
            header.push_str(&format!(",\"idle_time_limit\":{limit}"));
        }

        // Environment
        let has_env = config.env_shell.is_some() || config.env_term.is_some();
        if has_env {
            header.push_str(",\"env\":{");
            let mut first = true;
            if let Some(ref shell) = config.env_shell {
                header.push_str(&format!("\"SHELL\":\"{}\"", escape_json_string(shell)));
                first = false;
            }
            if let Some(ref term) = config.env_term {
                if !first {
                    header.push(',');
                }
                header.push_str(&format!("\"TERM\":\"{}\"", escape_json_string(term)));
            }
            header.push('}');
        }

        header.push('}');
        header
    }

    /// Record a terminal output event.
    ///
    /// The data is written as an "o" (output) event with the elapsed time
    /// since recording started.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn record_output(&mut self, data: &[u8]) -> io::Result<()> {
        self.record_event("o", data)
    }

    /// Record a terminal input event.
    ///
    /// The data is written as an "i" (input) event. This is only recorded
    /// if `record_input` was enabled in the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn record_input(&mut self, data: &[u8]) -> io::Result<()> {
        if self.config.record_input {
            self.record_event("i", data)
        } else {
            Ok(())
        }
    }

    /// Record an event with the given type and data.
    fn record_event(&mut self, event_type: &str, data: &[u8]) -> io::Result<()> {
        let mut time = self.start_time.elapsed().as_secs_f64();

        // Apply idle time limit if configured
        if let Some(limit) = self.config.idle_time_limit {
            let delta = time - self.last_time;
            if delta > limit {
                time = self.last_time + limit;
            }
        }
        self.last_time = time;

        // Escape the data as a JSON string
        let escaped = escape_bytes_to_json(data);

        // Format: [time, "type", "data"]
        writeln!(self.output, "[{time:.6},\"{event_type}\",\"{escaped}\"]")?;

        self.event_count += 1;
        Ok(())
    }

    /// Record output with a specific timestamp offset.
    ///
    /// Useful for replaying recorded data with preserved timing.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn record_output_at(&mut self, time: Duration, data: &[u8]) -> io::Result<()> {
        self.record_event_at(time.as_secs_f64(), "o", data)
    }

    /// Record an event at a specific time.
    fn record_event_at(&mut self, time: f64, event_type: &str, data: &[u8]) -> io::Result<()> {
        let escaped = escape_bytes_to_json(data);
        writeln!(self.output, "[{time:.6},\"{event_type}\",\"{escaped}\"]")?;
        self.event_count += 1;
        self.last_time = time;
        Ok(())
    }

    /// Finish recording and flush the output.
    ///
    /// Returns the total number of events recorded (excluding the header).
    ///
    /// # Errors
    ///
    /// Returns an error if flushing fails.
    pub fn finish(mut self) -> io::Result<u64> {
        self.output.flush()?;
        Ok(self.event_count)
    }

    /// Get the elapsed recording time.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get the current event count.
    #[must_use]
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Get the terminal dimensions.
    #[must_use]
    pub fn dimensions(&self) -> (u16, u16) {
        (self.config.width, self.config.height)
    }

    /// Resize the recording (updates internal state, does not write an event).
    ///
    /// Note: Asciicast v2 does not have a standard resize event, so this only
    /// affects the internal state. For recordings where size changes, consider
    /// using the initial maximum dimensions.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.config.width = width;
        self.config.height = height;
    }
}

// ============================================================================
// JSON Escaping
// ============================================================================

/// Escape a string for JSON output.
fn escape_json_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// Escape bytes to a JSON string, handling both valid UTF-8 and raw bytes.
fn escape_bytes_to_json(data: &[u8]) -> String {
    // Try to interpret as UTF-8 first for efficiency
    if let Ok(s) = std::str::from_utf8(data) {
        return escape_json_string(s);
    }

    // Fall back to byte-by-byte escaping for invalid UTF-8
    let mut escaped = String::with_capacity(data.len() * 2);
    let mut i = 0;

    while i < data.len() {
        // Try to decode a UTF-8 sequence
        let remaining = &data[i..];
        match std::str::from_utf8(remaining) {
            Ok(s) => {
                // Rest is valid UTF-8
                escaped.push_str(&escape_json_string(s));
                break;
            }
            Err(e) => {
                // Process valid portion
                let valid_up_to = e.valid_up_to();
                if valid_up_to > 0 {
                    // Safe: we know this portion is valid UTF-8 from the error
                    if let Ok(valid) = std::str::from_utf8(&remaining[..valid_up_to]) {
                        escaped.push_str(&escape_json_string(valid));
                    }
                }
                i += valid_up_to;

                // Escape the invalid byte
                if i < data.len() {
                    escaped.push_str(&format!("\\u{:04x}", data[i]));
                    i += 1;
                }
            }
        }
    }

    escaped
}

// ============================================================================
// Player / Loader (for reading asciicast files)
// ============================================================================

/// A single event from an asciicast recording.
#[derive(Debug, Clone)]
pub struct AsciicastEvent {
    /// Time offset in seconds from start of recording.
    pub time: f64,
    /// Event type: "o" for output, "i" for input.
    pub event_type: String,
    /// Event data.
    pub data: Vec<u8>,
}

/// Header information from an asciicast recording.
#[derive(Debug, Clone)]
pub struct AsciicastHeader {
    /// Format version (should be 2).
    pub version: u8,
    /// Terminal width.
    pub width: u16,
    /// Terminal height.
    pub height: u16,
    /// Recording timestamp (Unix seconds).
    pub timestamp: Option<u64>,
    /// Session title.
    pub title: Option<String>,
    /// Idle time limit.
    pub idle_time_limit: Option<f64>,
}

/// Load and iterate over events in an asciicast file.
pub struct AsciicastLoader<R> {
    reader: std::io::BufReader<R>,
    header: AsciicastHeader,
}

impl<R: io::Read> AsciicastLoader<R> {
    /// Load an asciicast file, parsing the header.
    ///
    /// # Errors
    ///
    /// Returns an error if the header cannot be parsed.
    pub fn new(reader: R) -> io::Result<Self> {
        use std::io::BufRead;

        let mut reader = std::io::BufReader::new(reader);
        let mut header_line = String::new();
        reader.read_line(&mut header_line)?;

        let header = Self::parse_header(&header_line)?;

        Ok(Self { reader, header })
    }

    /// Parse the header JSON.
    fn parse_header(line: &str) -> io::Result<AsciicastHeader> {
        // Simple JSON parsing without external deps
        let line = line.trim();
        if !line.starts_with('{') || !line.ends_with('}') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid asciicast header",
            ));
        }

        let mut version: u8 = 0;
        let mut width: u16 = 80;
        let mut height: u16 = 24;
        let mut timestamp: Option<u64> = None;
        let mut title: Option<String> = None;
        let mut idle_time_limit: Option<f64> = None;

        // Extract version
        if let Some(pos) = line.find("\"version\":") {
            let rest = &line[pos + 10..];
            if let Some(num) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                version = num.parse().unwrap_or(0);
            }
        }

        // Extract width
        if let Some(pos) = line.find("\"width\":") {
            let rest = &line[pos + 8..];
            if let Some(num) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                width = num.parse().unwrap_or(80);
            }
        }

        // Extract height
        if let Some(pos) = line.find("\"height\":") {
            let rest = &line[pos + 9..];
            if let Some(num) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                height = num.parse().unwrap_or(24);
            }
        }

        // Extract timestamp
        if let Some(pos) = line.find("\"timestamp\":") {
            let rest = &line[pos + 12..];
            if let Some(num) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                timestamp = num.parse().ok();
            }
        }

        // Extract title (simple case, no nested quotes)
        if let Some(pos) = line.find("\"title\":\"") {
            let rest = &line[pos + 9..];
            if let Some(end) = rest.find('"') {
                title = Some(rest[..end].to_string());
            }
        }

        // Extract idle_time_limit
        if let Some(pos) = line.find("\"idle_time_limit\":") {
            let rest = &line[pos + 18..];
            if let Some(num) = rest
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()
            {
                idle_time_limit = num.parse().ok();
            }
        }

        if version != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported asciicast version: {version}"),
            ));
        }

        Ok(AsciicastHeader {
            version,
            width,
            height,
            timestamp,
            title,
            idle_time_limit,
        })
    }

    /// Get the header information.
    #[must_use]
    pub fn header(&self) -> &AsciicastHeader {
        &self.header
    }

    /// Read the next event.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or parsing fails.
    pub fn next_event(&mut self) -> io::Result<Option<AsciicastEvent>> {
        use std::io::BufRead;

        let mut line = String::new();
        let bytes = self.reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }

        Self::parse_event(&line).map(Some)
    }

    /// Parse an event line.
    fn parse_event(line: &str) -> io::Result<AsciicastEvent> {
        let line = line.trim();
        if !line.starts_with('[') || !line.ends_with(']') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid event format",
            ));
        }

        // Parse: [time, "type", "data"]
        let inner = &line[1..line.len() - 1];

        // Find first comma (after time)
        let Some(comma1) = inner.find(',') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing time"));
        };
        let time_str = inner[..comma1].trim();
        let time: f64 = time_str.parse().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid time value")
        })?;

        // Find type
        let rest = &inner[comma1 + 1..];
        let Some(type_start) = rest.find('"') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing type"));
        };
        let rest = &rest[type_start + 1..];
        let Some(type_end) = rest.find('"') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing type end"));
        };
        let event_type = rest[..type_end].to_string();

        // Find data
        let rest = &rest[type_end + 1..];
        let Some(comma2) = rest.find(',') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing data"));
        };
        let rest = &rest[comma2 + 1..];
        let Some(data_start) = rest.find('"') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing data"));
        };
        let rest = &rest[data_start + 1..];

        // Find end quote (handling escapes)
        let data_str = unescape_json_string(rest)?;

        Ok(AsciicastEvent {
            time,
            event_type,
            data: data_str.into_bytes(),
        })
    }

    /// Load all events into a vector.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or parsing fails.
    pub fn load_all(&mut self) -> io::Result<Vec<AsciicastEvent>> {
        let mut events = Vec::new();
        while let Some(event) = self.next_event()? {
            events.push(event);
        }
        Ok(events)
    }
}

/// Unescape a JSON string.
fn unescape_json_string(s: &str) -> io::Result<String> {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '"' {
            // End of string
            break;
        }
        if c == '\\' {
            match chars.next() {
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('/') => result.push('/'),
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('u') => {
                    // Unicode escape: \uXXXX
                    let mut code = String::with_capacity(4);
                    for _ in 0..4 {
                        if let Some(hex) = chars.next() {
                            code.push(hex);
                        }
                    }
                    if let Ok(n) = u32::from_str_radix(&code, 16) {
                        if let Some(ch) = char::from_u32(n) {
                            result.push(ch);
                        }
                    }
                }
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn recorder_writes_header() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let _recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.starts_with("{\"version\":2"));
        assert!(data.contains("\"width\":80"));
        assert!(data.contains("\"height\":24"));
        assert!(data.contains("\"timestamp\":"));
    }

    #[test]
    fn recorder_writes_output_events() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_output(b"Hello").unwrap();
        recorder.finish().unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = data.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\"o\""));
        assert!(lines[1].contains("\"Hello\""));
    }

    #[test]
    fn recorder_escapes_special_chars() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_output(b"line1\nline2\ttab").unwrap();
        recorder.finish().unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.contains("\\n"));
        assert!(data.contains("\\t"));
    }

    #[test]
    fn recorder_escapes_ansi_codes() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_output(b"\x1b[31mred\x1b[0m").unwrap();
        recorder.finish().unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        // ESC should be escaped as \u001b
        assert!(data.contains("\\u001b"));
    }

    #[test]
    fn recorder_input_disabled_by_default() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_input(b"ignored").unwrap();
        let count = recorder.finish().unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn recorder_input_when_enabled() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24).with_input_recording(true);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_input(b"command").unwrap();
        let count = recorder.finish().unwrap();

        assert_eq!(count, 1);
        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.contains("\"i\""));
    }

    #[test]
    fn recorder_with_title() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24).with_title("My Session");
        let _recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.contains("\"title\":\"My Session\""));
    }

    #[test]
    fn recorder_with_env() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24)
            .with_env_shell("/bin/bash")
            .with_env_term("xterm-256color");
        let _recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.contains("\"SHELL\":\"/bin/bash\""));
        assert!(data.contains("\"TERM\":\"xterm-256color\""));
    }

    #[test]
    fn recorder_idle_time_limit() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24).with_idle_time_limit(2.5);
        let _recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        let data = String::from_utf8(output.into_inner()).unwrap();
        assert!(data.contains("\"idle_time_limit\":2.5"));
    }

    #[test]
    fn recorder_event_count() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_output(b"one").unwrap();
        recorder.record_output(b"two").unwrap();
        recorder.record_output(b"three").unwrap();

        assert_eq!(recorder.event_count(), 3);
        let count = recorder.finish().unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn loader_parses_header() {
        let data = "{\"version\":2,\"width\":100,\"height\":50,\"timestamp\":1234567890}\n";
        let loader = AsciicastLoader::new(data.as_bytes()).unwrap();
        let header = loader.header();

        assert_eq!(header.version, 2);
        assert_eq!(header.width, 100);
        assert_eq!(header.height, 50);
        assert_eq!(header.timestamp, Some(1234567890));
    }

    #[test]
    fn loader_parses_events() {
        let data = "{\"version\":2,\"width\":80,\"height\":24}\n\
                    [0.5,\"o\",\"Hello\"]\n\
                    [1.0,\"o\",\"World\"]\n";
        let mut loader = AsciicastLoader::new(data.as_bytes()).unwrap();

        let event1 = loader.next_event().unwrap().unwrap();
        assert!((event1.time - 0.5).abs() < 0.001);
        assert_eq!(event1.event_type, "o");
        assert_eq!(event1.data, b"Hello");

        let event2 = loader.next_event().unwrap().unwrap();
        assert!((event2.time - 1.0).abs() < 0.001);
        assert_eq!(event2.data, b"World");

        assert!(loader.next_event().unwrap().is_none());
    }

    #[test]
    fn loader_handles_escapes() {
        let data = "{\"version\":2,\"width\":80,\"height\":24}\n\
                    [0.1,\"o\",\"line1\\nline2\"]\n";
        let mut loader = AsciicastLoader::new(data.as_bytes()).unwrap();

        let event = loader.next_event().unwrap().unwrap();
        assert_eq!(event.data, b"line1\nline2");
    }

    #[test]
    fn loader_handles_unicode_escapes() {
        let data = "{\"version\":2,\"width\":80,\"height\":24}\n\
                    [0.1,\"o\",\"\\u001b[31mred\\u001b[0m\"]\n";
        let mut loader = AsciicastLoader::new(data.as_bytes()).unwrap();

        let event = loader.next_event().unwrap().unwrap();
        assert_eq!(event.data, b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn loader_load_all() {
        let data = "{\"version\":2,\"width\":80,\"height\":24}\n\
                    [0.1,\"o\",\"A\"]\n\
                    [0.2,\"i\",\"B\"]\n\
                    [0.3,\"o\",\"C\"]\n";
        let mut loader = AsciicastLoader::new(data.as_bytes()).unwrap();

        let events = loader.load_all().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "o");
        assert_eq!(events[1].event_type, "i");
        assert_eq!(events[2].event_type, "o");
    }

    #[test]
    fn roundtrip() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(80, 24).with_title("Test");
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        recorder.record_output_at(Duration::from_millis(100), b"Hello").unwrap();
        recorder.record_output_at(Duration::from_millis(200), b"World").unwrap();
        recorder.finish().unwrap();

        let data = output.into_inner();
        let mut loader = AsciicastLoader::new(data.as_slice()).unwrap();

        assert_eq!(loader.header().width, 80);
        assert_eq!(loader.header().height, 24);
        assert_eq!(loader.header().title.as_deref(), Some("Test"));

        let events = loader.load_all().unwrap();
        assert_eq!(events.len(), 2);
        assert!((events[0].time - 0.1).abs() < 0.001);
        assert_eq!(events[0].data, b"Hello");
        assert!((events[1].time - 0.2).abs() < 0.001);
        assert_eq!(events[1].data, b"World");
    }

    #[test]
    fn escape_json_string_basic() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("a\"b"), "a\\\"b");
        assert_eq!(escape_json_string("a\\b"), "a\\\\b");
        assert_eq!(escape_json_string("a\nb"), "a\\nb");
        assert_eq!(escape_json_string("a\tb"), "a\\tb");
    }

    #[test]
    fn escape_bytes_handles_invalid_utf8() {
        let data = b"valid\xfftext";
        let escaped = escape_bytes_to_json(data);
        // Invalid byte 0xff should be escaped
        assert!(escaped.contains("\\u00ff"));
    }

    #[test]
    fn unescape_json_string_basic() {
        assert_eq!(unescape_json_string("hello\"").unwrap(), "hello");
        assert_eq!(unescape_json_string("a\\\"b\"").unwrap(), "a\"b");
        assert_eq!(unescape_json_string("a\\nb\"").unwrap(), "a\nb");
        assert_eq!(unescape_json_string("\\u0041\"").unwrap(), "A");
    }

    #[test]
    fn recorder_dimensions() {
        let mut output = Cursor::new(Vec::new());
        let config = RecordConfig::new(120, 40);
        let mut recorder = AsciicastRecorder::new(&mut output, config).unwrap();

        assert_eq!(recorder.dimensions(), (120, 40));
        recorder.resize(80, 24);
        assert_eq!(recorder.dimensions(), (80, 24));
    }
}
