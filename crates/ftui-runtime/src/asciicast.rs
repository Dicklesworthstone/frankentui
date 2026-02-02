#![forbid(unsafe_code)]

//! Asciicast v2 recorder for capturing terminal sessions.
//!
//! This recorder writes newline-delimited JSON (NDJSON) compatible with
//! asciinema-player. The first line is the header object, followed by event
//! arrays of the form `[time, "o", "text"]` for output and `[time, "i", "text"]`
//! for input (optional).
//!
//! # Example
//! ```no_run
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//! use ftui_runtime::asciicast::{AsciicastRecorder, AsciicastWriter};
//! use ftui_runtime::{ScreenMode, TerminalWriter, UiAnchor};
//! use std::io::Cursor;
//!
//! let recorder = AsciicastRecorder::with_writer(Cursor::new(Vec::new()), 80, 24, 0).unwrap();
//! let output = Cursor::new(Vec::new());
//! let recording_output = AsciicastWriter::new(output, recorder);
//! let caps = TerminalCapabilities::detect();
//! let mut writer = TerminalWriter::new(recording_output, ScreenMode::Inline { ui_height: 10 }, UiAnchor::Bottom, caps);
//! writer.write_log("hello\n").unwrap();
//! ```

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{info, trace};

/// Records terminal output in asciicast v2 format.
#[derive(Debug)]
pub struct AsciicastRecorder<W: Write> {
    output: W,
    start: Instant,
    width: u16,
    height: u16,
    event_count: u64,
    path: Option<PathBuf>,
}

impl AsciicastRecorder<BufWriter<File>> {
    /// Create a recorder that writes to a file at `path`.
    pub fn new(path: &Path, width: u16, height: u16) -> io::Result<Self> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        let timestamp = unix_timestamp()?;
        let recorder =
            AsciicastRecorder::build(writer, width, height, timestamp, Some(path.to_path_buf()))?;
        info!(
            path = ?path,
            width = width,
            height = height,
            "Asciicast recording started"
        );
        Ok(recorder)
    }
}

impl<W: Write> AsciicastRecorder<W> {
    /// Create a recorder that writes to the provided writer.
    ///
    /// `timestamp` is seconds since UNIX epoch used in the asciicast header.
    pub fn with_writer(output: W, width: u16, height: u16, timestamp: i64) -> io::Result<Self> {
        let recorder = Self::build(output, width, height, timestamp, None)?;
        info!(
            width = width,
            height = height,
            timestamp = timestamp,
            "Asciicast recording started"
        );
        Ok(recorder)
    }

    /// Record terminal output bytes.
    pub fn record_output(&mut self, data: &[u8]) -> io::Result<()> {
        self.record_event("o", data)
    }

    /// Record terminal input bytes (optional).
    pub fn record_input(&mut self, data: &[u8]) -> io::Result<()> {
        self.record_event("i", data)
    }

    /// Number of events recorded so far.
    #[must_use]
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Elapsed duration since recording started.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.start.elapsed()
    }

    /// Returns the terminal width recorded in the asciicast header.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Returns the terminal height recorded in the asciicast header.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Flush output and return the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        let duration = self.start.elapsed().as_secs_f64();
        self.output.flush()?;
        if let Some(path) = &self.path {
            info!(
                path = ?path,
                duration_secs = duration,
                events = self.event_count,
                "Asciicast recording complete"
            );
        } else {
            info!(
                duration_secs = duration,
                events = self.event_count,
                "Asciicast recording complete"
            );
        }
        Ok(self.output)
    }

    fn record_event(&mut self, kind: &str, data: &[u8]) -> io::Result<()> {
        let time = self.start.elapsed().as_secs_f64();
        let text = String::from_utf8_lossy(data);
        let escaped = escape_json(&text);
        writeln!(self.output, "[{:.6},\"{}\",\"{}\"]", time, kind, escaped)?;
        self.event_count += 1;
        trace!(
            bytes = data.len(),
            elapsed_secs = time,
            kind = kind,
            "Output recorded"
        );
        Ok(())
    }

    fn build(
        mut output: W,
        width: u16,
        height: u16,
        timestamp: i64,
        path: Option<PathBuf>,
    ) -> io::Result<Self> {
        write_header(&mut output, width, height, timestamp)?;
        Ok(Self {
            output,
            start: Instant::now(),
            width,
            height,
            event_count: 0,
            path,
        })
    }
}

/// Writer that mirrors terminal output into an asciicast recorder.
#[derive(Debug)]
pub struct AsciicastWriter<W: Write, R: Write> {
    inner: W,
    recorder: AsciicastRecorder<R>,
}

impl<W: Write, R: Write> AsciicastWriter<W, R> {
    /// Create a new recording writer.
    pub const fn new(inner: W, recorder: AsciicastRecorder<R>) -> Self {
        Self { inner, recorder }
    }

    /// Access the underlying recorder (for input recording).
    pub fn recorder_mut(&mut self) -> &mut AsciicastRecorder<R> {
        &mut self.recorder
    }

    /// Record input bytes.
    pub fn record_input(&mut self, data: &[u8]) -> io::Result<()> {
        self.recorder.record_input(data)
    }

    /// Flush and finish recording, returning the inner writer and recorder output.
    pub fn finish(mut self) -> io::Result<(W, R)> {
        self.inner.flush()?;
        let recorder_output = self.recorder.finish()?;
        Ok((self.inner, recorder_output))
    }
}

impl<W: Write, R: Write> Write for AsciicastWriter<W, R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        if written > 0 {
            self.recorder.record_output(&buf[..written])?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        self.recorder.output.flush()
    }
}

fn write_header<W: Write>(
    output: &mut W,
    width: u16,
    height: u16,
    timestamp: i64,
) -> io::Result<()> {
    writeln!(
        output,
        "{{\"version\":2,\"width\":{},\"height\":{},\"timestamp\":{}}}",
        width, height, timestamp
    )
}

fn unix_timestamp() -> io::Result<i64> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("system time before unix epoch"))?;
    Ok(since_epoch.as_secs() as i64)
}

fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '\"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_recorder(width: u16, height: u16) -> AsciicastRecorder<Cursor<Vec<u8>>> {
        AsciicastRecorder::with_writer(Cursor::new(Vec::new()), width, height, 0).unwrap()
    }

    fn output_string(recorder: AsciicastRecorder<Cursor<Vec<u8>>>) -> String {
        let cursor = recorder.finish().unwrap();
        String::from_utf8(cursor.into_inner()).unwrap()
    }

    // --- Header tests ---

    #[test]
    fn header_and_output_are_written() {
        let cursor = Cursor::new(Vec::new());
        let mut recorder = AsciicastRecorder::with_writer(cursor, 80, 24, 123).unwrap();
        recorder.record_output(b"hi\n").unwrap();
        let cursor = recorder.finish().unwrap();
        let output = String::from_utf8(cursor.into_inner()).unwrap();
        let mut lines = output.lines();
        assert_eq!(
            lines.next().unwrap(),
            "{\"version\":2,\"width\":80,\"height\":24,\"timestamp\":123}"
        );
        let event = lines.next().unwrap();
        assert!(event.contains("\"o\""));
        assert!(event.contains("hi\\n"));
    }

    #[test]
    fn header_contains_version_2() {
        let recorder = make_recorder(40, 10);
        let output = output_string(recorder);
        let header = output.lines().next().unwrap();
        assert!(header.contains("\"version\":2"));
    }

    #[test]
    fn header_contains_dimensions() {
        let recorder = make_recorder(120, 50);
        let output = output_string(recorder);
        let header = output.lines().next().unwrap();
        assert!(header.contains("\"width\":120"));
        assert!(header.contains("\"height\":50"));
    }

    // --- Event recording tests ---

    #[test]
    fn record_output_creates_output_event() {
        let mut recorder = make_recorder(80, 24);
        recorder.record_output(b"hello").unwrap();
        let output = output_string(recorder);
        let event = output.lines().nth(1).unwrap();
        assert!(event.starts_with('['));
        assert!(event.contains("\"o\""));
        assert!(event.contains("hello"));
    }

    #[test]
    fn record_input_creates_input_event() {
        let mut recorder = make_recorder(80, 24);
        recorder.record_input(b"key").unwrap();
        let output = output_string(recorder);
        let event = output.lines().nth(1).unwrap();
        assert!(event.contains("\"i\""));
        assert!(event.contains("key"));
    }

    #[test]
    fn multiple_events_are_sequential() {
        let mut recorder = make_recorder(80, 24);
        recorder.record_output(b"first").unwrap();
        recorder.record_output(b"second").unwrap();
        recorder.record_input(b"third").unwrap();
        let output = output_string(recorder);
        let lines: Vec<&str> = output.lines().collect();
        // header + 3 events
        assert_eq!(lines.len(), 4);
        assert!(lines[1].contains("first"));
        assert!(lines[2].contains("second"));
        assert!(lines[3].contains("third"));
    }

    #[test]
    fn event_count_tracks_events() {
        let mut recorder = make_recorder(80, 24);
        assert_eq!(recorder.event_count(), 0);
        recorder.record_output(b"a").unwrap();
        assert_eq!(recorder.event_count(), 1);
        recorder.record_input(b"b").unwrap();
        assert_eq!(recorder.event_count(), 2);
    }

    #[test]
    fn accessor_methods_return_dimensions() {
        let recorder = make_recorder(132, 43);
        assert_eq!(recorder.width(), 132);
        assert_eq!(recorder.height(), 43);
    }

    #[test]
    fn duration_is_non_negative() {
        let recorder = make_recorder(80, 24);
        assert!(recorder.duration().as_secs_f64() >= 0.0);
    }

    // --- JSON escaping tests ---

    #[test]
    fn json_escape_controls() {
        let cursor = Cursor::new(Vec::new());
        let mut recorder = AsciicastRecorder::with_writer(cursor, 1, 1, 0).unwrap();
        recorder.record_output(b"\"\\\\\n").unwrap();
        let cursor = recorder.finish().unwrap();
        let output = String::from_utf8(cursor.into_inner()).unwrap();
        let event = output.lines().nth(1).unwrap();
        assert!(event.contains("\\\"\\\\\\\\\\n"));
    }

    #[test]
    fn escape_json_handles_all_special_chars() {
        assert_eq!(escape_json("\""), "\\\"");
        assert_eq!(escape_json("\\"), "\\\\");
        assert_eq!(escape_json("\n"), "\\n");
        assert_eq!(escape_json("\r"), "\\r");
        assert_eq!(escape_json("\t"), "\\t");
        assert_eq!(escape_json("\u{08}"), "\\b");
        assert_eq!(escape_json("\u{0C}"), "\\f");
    }

    #[test]
    fn escape_json_passes_normal_text() {
        assert_eq!(escape_json("hello world"), "hello world");
        assert_eq!(escape_json(""), "");
    }

    #[test]
    fn escape_json_handles_low_control_chars() {
        let result = escape_json("\x01\x02");
        assert!(result.contains("\\u0001"));
        assert!(result.contains("\\u0002"));
    }

    // --- AsciicastWriter tests ---

    #[test]
    fn writer_mirrors_output_to_recorder() {
        let output = Cursor::new(Vec::new());
        let recorder = make_recorder(80, 24);
        let mut writer = AsciicastWriter::new(output, recorder);

        writer.write_all(b"test data").unwrap();
        writer.flush().unwrap();

        let (output, recording) = writer.finish().unwrap();
        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let recording_str = String::from_utf8(recording.into_inner()).unwrap();

        assert_eq!(output_str, "test data");
        assert!(recording_str.contains("test data"));
    }

    #[test]
    fn writer_record_input_works() {
        let output = Cursor::new(Vec::new());
        let recorder = make_recorder(80, 24);
        let mut writer = AsciicastWriter::new(output, recorder);

        writer.record_input(b"key press").unwrap();

        let (_, recording) = writer.finish().unwrap();
        let recording_str = String::from_utf8(recording.into_inner()).unwrap();
        assert!(recording_str.contains("\"i\""));
        assert!(recording_str.contains("key press"));
    }

    #[test]
    fn writer_recorder_mut_accessible() {
        let output = Cursor::new(Vec::new());
        let recorder = make_recorder(80, 24);
        let mut writer = AsciicastWriter::new(output, recorder);

        assert_eq!(writer.recorder_mut().event_count(), 0);
        writer.write_all(b"x").unwrap();
        assert_eq!(writer.recorder_mut().event_count(), 1);
    }

    // --- Empty recording test ---

    #[test]
    fn finish_with_no_events_produces_header_only() {
        let recorder = make_recorder(80, 24);
        let output = output_string(recorder);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1); // header only
    }
}
