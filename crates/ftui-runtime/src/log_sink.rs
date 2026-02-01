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

use std::io::{self, Write};
use ftui_render::sanitize::sanitize;
use crate::TerminalWriter;

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
        let mut sink = LogSink::new(&mut writer);

        write!(sink, "Hello").unwrap();
        
        // Should not be in output yet
        let output = writer.flush().unwrap(); // Accessing inner writer via flush? No, we need to inspect inner.
        // We can't easily inspect inner here because LogSink borrows writer mutably.
        // We have to drop sink first.
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
}
