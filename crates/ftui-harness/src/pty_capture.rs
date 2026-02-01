#![forbid(unsafe_code)]

//! Harness helper for routing subprocess PTY output through the one-writer path.
//!
//! This module is feature-gated behind `pty-capture` and is intended for
//! harness-style integrations that need to run tools while keeping inline
//! mode stable (sanitize-by-default + log routing).

use ftui_extras::pty_capture::{PtyCapture, PtyCaptureConfig};
use ftui_runtime::log_sink::LogSink;
use ftui_runtime::terminal_writer::TerminalWriter;
use portable_pty::CommandBuilder;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// Run a command in a PTY and stream its output through a [`LogSink`].
///
/// This preserves sanitize-by-default (via `LogSink`) and keeps terminal
/// output within the one-writer path.
pub fn run_command_with_pty<W: Write>(
    writer: &mut TerminalWriter<W>,
    cmd: CommandBuilder,
    config: PtyCaptureConfig,
) -> io::Result<portable_pty::ExitStatus> {
    let mut capture = PtyCapture::spawn(config, cmd)?;
    let mut sink = LogSink::new(writer);

    loop {
        let drained = capture.drain_to_log_sink(&mut sink)?;
        if drained == 0 {
            if capture.is_eof() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    sink.flush()?;
    capture.wait()
}

#[cfg(all(test, feature = "pty-capture", unix))]
mod tests {
    use super::*;
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use ftui_runtime::terminal_writer::{ScreenMode, UiAnchor};

    fn create_writer() -> TerminalWriter<Vec<u8>> {
        TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        )
    }

    #[test]
    fn harness_pty_capture_sanitizes() {
        let mut writer = create_writer();
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'ok \\x1b[31mred\\x1b[0m\\n'"]);

        let _ = run_command_with_pty(&mut writer, cmd, PtyCaptureConfig::default());

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("ok red"));
        assert!(!output_str.contains("\x1b[31m"));
    }
}
