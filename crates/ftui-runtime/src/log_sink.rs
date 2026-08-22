#![forbid(unsafe_code)]

//! Log sink for in-process output routing.
//!
//! The `LogSink` struct implements [`std::io::Write`] and forwards output to
//! [`TerminalWriter::write_log`], ensuring that:
//!
//! 1. Output is line-buffered (to prevent torn lines).
//! 2. Content is sanitized (escape sequences stripped) by default.
//! 3. The One-Writer Rule is respected.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::log_sink::LogSink;
//! use std::io::Write;
//!
//! // Assuming you have a mutable reference to TerminalWriter
//! let mut sink = LogSink::new(&mut terminal_writer);
//!
//! // Now you can use it with any std::io::Write consumer
//! writeln!(sink, "This log message is safe: \x1b[31mcolors stripped\x1b[0m").unwrap();
//! ```

use crate::TerminalWriter;
use ftui_render::sanitize::sanitize;
use std::io::{self, Write};

/// A write adapter that routes output to the terminal's log scrollback.
///
/// Wraps a mutable reference to [`TerminalWriter`] and implements [`std::io::Write`].
/// Buffers partial lines until a newline is encountered or `flush()` is called.
pub struct LogSink<'a, W: Write> {
    writer: &'a mut TerminalWriter<W>,
    buffer: Vec<u8>,
}

impl<'a, W: Write> LogSink<'a, W> {
    /// Create a new log sink wrapping the given terminal writer.
    pub fn new(writer: &'a mut TerminalWriter<W>) -> Self {
        Self {
            writer,
            buffer: Vec::with_capacity(1024),
        }
    }
}

impl<W: Write> Write for LogSink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            if byte == b'\n' {
                // Found a newline, flush the buffer
                let line = String::from_utf8_lossy(&self.buffer);
                let safe_line = sanitize(&line);

                // Write line + newline to terminal writer
                // We format manually to ensure we own the string if needed
                self.writer.write_log(&format!("{}\n", safe_line))?;

                self.buffer.clear();
            } else {
                self.buffer.push(byte);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            // Flush remaining buffer as a partial line
            let line = String::from_utf8_lossy(&self.buffer);
            let safe_line = sanitize(&line);
            self.writer.write_log(&safe_line)?;
            self.buffer.clear();
        }
        self.writer.flush()
    }
}

impl<W: Write> Drop for LogSink<'_, W> {
    fn drop(&mut self) {
        // Best-effort flush on drop.
        // We ignore errors here because we can't propagate them and panicking in drop is bad.
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_writer::{ScreenMode, UiAnchor};
    use ftui_core::terminal_capabilities::TerminalCapabilities;

    // Helper to create a dummy writer
    fn create_writer() -> TerminalWriter<Vec<u8>> {
        TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        )
    }

    #[test]
    fn log_sink_buffers_lines() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            write!(sink, "Hello").unwrap();
            // Not dropped yet, so buffer holds "Hello"
        }
        // Dropped now, should flush

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        // With Drop flush implemented, partial lines ARE written.
        assert!(
            output_str.contains("Hello"),
            "partial content should be flushed on drop"
        );
    }

    #[test]
    fn log_sink_sanitizes_output() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            writeln!(sink, "Unsafe \x1b[31mred\x1b[0m text").unwrap();
        }

        // writer.flush() writes to the internal buffer
        // We need to consume writer to check output
        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("Unsafe red text"));
        assert!(!output_str.contains("\x1b[31m"));
    }

    #[test]
    fn log_sink_flushes_partial_line() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            write!(sink, "Partial").unwrap();
            sink.flush().unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("Partial"));
    }

    #[test]
    fn log_sink_multiple_lines() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            writeln!(sink, "Line1").unwrap();
            writeln!(sink, "Line2").unwrap();
            writeln!(sink, "Line3").unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("Line1"));
        assert!(output_str.contains("Line2"));
        assert!(output_str.contains("Line3"));
    }

    #[test]
    fn log_sink_empty_write() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            let n = sink.write(b"").unwrap();
            assert_eq!(n, 0);
        }
    }

    #[test]
    fn log_sink_newline_only() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            sink.write_all(b"\n").unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        // An empty line carries no payload: overlay-mode write_log emits
        // nothing for it and NEVER a raw LF byte (bd-th0p6 — an unqualified
        // LF would scroll and corrupt the displayed UI).
        assert!(
            !output_str.contains('\n'),
            "overlay log emission must not contain raw newline bytes"
        );
    }

    #[test]
    fn log_sink_multiple_newlines_in_one_write() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            sink.write_all(b"A\nB\nC\n").unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains('A'));
        assert!(output_str.contains('B'));
        assert!(output_str.contains('C'));
    }

    #[test]
    fn log_sink_sanitizes_multiple_escapes() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            writeln!(sink, "\x1b[31mRed\x1b[0m \x1b[1mBold\x1b[0m").unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // The content "Red Bold" should appear (sanitized)
        assert!(output_str.contains("Red"));
        assert!(output_str.contains("Bold"));
        // The original SGR sequences (31m, 0m, 1m) should be stripped from content
        // Note: terminal writer adds its own cursor control sequences, so we check
        // that the specific SGR codes from the input are not present
        assert!(!output_str.contains("\x1b[31m"));
        assert!(!output_str.contains("\x1b[1m"));
    }

    #[test]
    fn log_sink_invalid_utf8_lossy() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            // Write some invalid UTF-8 bytes followed by a newline
            sink.write_all(&[0xFF, 0xFE, b'\n']).unwrap();
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        // Should contain replacement characters, not panic
        assert!(output_str.contains('\u{FFFD}') || !output_str.is_empty());
    }

    #[test]
    fn log_sink_drop_without_flush_writes_partial() {
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            write!(sink, "NoNewline").unwrap();
            // Drop without flush
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        // With Drop flush, partial line should appear
        assert!(
            output_str.contains("NoNewline"),
            "partial line should be written on drop"
        );
    }

    #[test]
    fn log_sink_write_returns_full_length() {
        let mut writer = create_writer();
        let mut sink = LogSink::new(&mut writer);
        let data = b"Hello World\n";
        let n = sink.write(data).unwrap();
        assert_eq!(n, data.len());
    }
}
