#![forbid(unsafe_code)]

//! PTY capture utilities for routing subprocess output through the one-writer path.
//!
//! This module is feature-gated (`pty-capture`) and intended for harness-style
//! integrations that need to execute subprocesses while preserving terminal
//! correctness (sanitize-by-default + inline redraw safety).

use ftui_runtime::log_sink::LogSink;
use portable_pty::{CommandBuilder, ExitStatus, PtySize};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Configuration for PTY capture sessions.
#[derive(Debug, Clone)]
pub struct PtyCaptureConfig {
    /// PTY width in columns.
    pub cols: u16,
    /// PTY height in rows.
    pub rows: u16,
    /// TERM to set in the child (defaults to xterm-256color).
    pub term: Option<String>,
    /// Extra environment variables to set in the child.
    pub env: Vec<(String, String)>,
}

impl Default for PtyCaptureConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            term: Some("xterm-256color".to_string()),
            env: Vec::new(),
        }
    }
}

impl PtyCaptureConfig {
    /// Override PTY dimensions.
    pub fn with_size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Override TERM in the child.
    pub fn with_term(mut self, term: impl Into<String>) -> Self {
        self.term = Some(term.into());
        self
    }

    /// Add an environment variable in the child.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

#[derive(Debug)]
enum ReaderMsg {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

/// A PTY-backed subprocess with non-blocking output capture.
pub struct PtyCapture {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<ReaderMsg>,
    reader_thread: Option<thread::JoinHandle<()>>,
    eof: bool,
}

impl fmt::Debug for PtyCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyCapture")
            .field("child_pid", &self.child.process_id())
            .field("eof", &self.eof)
            .finish()
    }
}

impl PtyCapture {
    /// Spawn a child process attached to a new PTY.
    pub fn spawn(mut config: PtyCaptureConfig, mut cmd: CommandBuilder) -> io::Result<Self> {
        if let Some(term) = config.term.take() {
            cmd.env("TERM", term);
        }
        for (k, v) in config.env.drain(..) {
            cmd.env(k, v);
        }

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(portable_pty_error)?;

        let child = pair.slave.spawn_command(cmd).map_err(portable_pty_error)?;
        let mut reader = pair.master.try_clone_reader().map_err(portable_pty_error)?;
        let writer = pair.master.take_writer().map_err(portable_pty_error)?;

        let (tx, rx) = mpsc::sync_channel::<ReaderMsg>(1024);
        let reader_thread = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(ReaderMsg::Eof);
                        break;
                    }
                    Ok(n) => {
                        if tx.send(ReaderMsg::Data(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(ReaderMsg::Err(err));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            writer,
            rx,
            reader_thread: Some(reader_thread),
            eof: false,
        })
    }

    /// Read any available output without blocking.
    pub fn read_available(&mut self) -> io::Result<Vec<u8>> {
        self.read_available_with_timeout(Duration::from_millis(0))
    }

    /// Read available output, waiting up to `timeout` for the first chunk.
    pub fn read_available_with_timeout(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        if self.eof {
            return Ok(Vec::new());
        }

        let first = if timeout.is_zero() {
            self.rx.try_recv().ok()
        } else {
            self.rx.recv_timeout(timeout).ok()
        };

        let mut msg = match first {
            Some(msg) => msg,
            None => return Ok(Vec::new()),
        };

        let mut output = Vec::new();

        loop {
            match msg {
                ReaderMsg::Data(bytes) => output.extend_from_slice(&bytes),
                ReaderMsg::Eof => {
                    self.eof = true;
                    break;
                }
                ReaderMsg::Err(err) => return Err(err),
            }

            match self.rx.try_recv() {
                Ok(next) => msg = next,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.eof = true;
                    break;
                }
            }
        }

        Ok(output)
    }

    /// Drain captured output into a [`LogSink`], preserving sanitize-by-default.
    pub fn drain_to_log_sink<W: Write>(&mut self, sink: &mut LogSink<W>) -> io::Result<usize> {
        let output = self.read_available()?;
        if output.is_empty() {
            return Ok(0);
        }

        sink.write_all(&output)?;
        Ok(output.len())
    }

    /// Send input bytes to the child process.
    pub fn send_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Wait for the child to exit and return its status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Child process id (if available on this platform).
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Whether the PTY reader observed EOF.
    pub fn is_eof(&self) -> bool {
        self.eof
    }
}

impl Drop for PtyCapture {
    fn drop(&mut self) {
        let _ = self.writer.flush();
        let _ = self.child.kill();

        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn portable_pty_error<E: fmt::Display>(err: E) -> io::Error {
    io::Error::other(err.to_string())
}

#[cfg(all(test, feature = "pty-capture", unix))]
mod tests {
    use super::*;
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use ftui_runtime::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
    use std::time::Duration;

    fn create_writer() -> TerminalWriter<Vec<u8>> {
        TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        )
    }

    #[test]
    fn pty_capture_reads_output() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf hello-pty"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let output = capture
            .read_available_with_timeout(Duration::from_secs(2))
            .unwrap();

        assert!(
            output
                .windows(b"hello-pty".len())
                .any(|w| w == b"hello-pty")
        );
    }

    #[test]
    fn pty_capture_routes_through_log_sink() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'ok \\\\x1b[31mred\\\\x1b[0m\\n'"]);

        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();
        let mut writer = create_writer();
        let mut sink = LogSink::new(&mut writer);

        let _ = capture.drain_to_log_sink(&mut sink).expect("drain to sink");
        sink.flush().expect("flush sink");

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("ok red"));
        assert!(!output_str.contains("\x1b[31m"));
    }
}
