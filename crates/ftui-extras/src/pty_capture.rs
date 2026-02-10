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
    #[must_use]
    pub fn with_size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Override TERM in the child.
    #[must_use]
    pub fn with_term(mut self, term: impl Into<String>) -> Self {
        self.term = Some(term.into());
        self
    }

    /// Add an environment variable in the child.
    #[must_use]
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

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = PtyCaptureConfig::default();
        assert_eq!(config.cols, 80);
        assert_eq!(config.rows, 24);
        assert_eq!(config.term, Some("xterm-256color".to_string()));
        assert!(config.env.is_empty());
    }

    #[test]
    fn with_size_overrides_dimensions() {
        let config = PtyCaptureConfig::default().with_size(120, 50);
        assert_eq!(config.cols, 120);
        assert_eq!(config.rows, 50);
    }

    #[test]
    fn with_term_overrides_terminal() {
        let config = PtyCaptureConfig::default().with_term("dumb");
        assert_eq!(config.term, Some("dumb".to_string()));
    }

    #[test]
    fn with_env_appends() {
        let config = PtyCaptureConfig::default()
            .with_env("KEY1", "VAL1")
            .with_env("KEY2", "VAL2");
        assert_eq!(config.env.len(), 2);
        assert_eq!(config.env[0], ("KEY1".to_string(), "VAL1".to_string()));
        assert_eq!(config.env[1], ("KEY2".to_string(), "VAL2".to_string()));
    }

    #[test]
    fn config_debug_impl() {
        let config = PtyCaptureConfig::default();
        let s = format!("{config:?}");
        assert!(s.contains("PtyCaptureConfig"));
    }

    #[test]
    fn config_clone() {
        let config = PtyCaptureConfig::default()
            .with_size(100, 40)
            .with_env("A", "B");
        let cloned = config.clone();
        assert_eq!(cloned.cols, 100);
        assert_eq!(cloned.rows, 40);
        assert_eq!(cloned.env.len(), 1);
    }

    #[test]
    fn with_size_zero() {
        let config = PtyCaptureConfig::default().with_size(0, 0);
        assert_eq!(config.cols, 0);
        assert_eq!(config.rows, 0);
    }

    #[test]
    fn with_size_large() {
        let config = PtyCaptureConfig::default().with_size(500, 200);
        assert_eq!(config.cols, 500);
        assert_eq!(config.rows, 200);
    }

    #[test]
    fn with_term_empty_string() {
        let config = PtyCaptureConfig::default().with_term("");
        assert_eq!(config.term, Some(String::new()));
    }

    #[test]
    fn with_env_preserves_order() {
        let config = PtyCaptureConfig::default()
            .with_env("C", "3")
            .with_env("A", "1")
            .with_env("B", "2");
        assert_eq!(config.env[0].0, "C");
        assert_eq!(config.env[1].0, "A");
        assert_eq!(config.env[2].0, "B");
    }

    #[test]
    fn with_env_allows_duplicate_keys() {
        let config = PtyCaptureConfig::default()
            .with_env("KEY", "val1")
            .with_env("KEY", "val2");
        assert_eq!(config.env.len(), 2);
    }

    #[test]
    fn builder_chaining_all_methods() {
        let config = PtyCaptureConfig::default()
            .with_size(132, 43)
            .with_term("screen")
            .with_env("LANG", "en_US.UTF-8")
            .with_env("LC_ALL", "C");
        assert_eq!(config.cols, 132);
        assert_eq!(config.rows, 43);
        assert_eq!(config.term, Some("screen".to_string()));
        assert_eq!(config.env.len(), 2);
    }

    #[test]
    fn default_does_not_set_env() {
        let config = PtyCaptureConfig::default();
        assert!(config.env.is_empty());
    }

    #[test]
    fn default_term_is_xterm_256color() {
        let config = PtyCaptureConfig::default();
        assert_eq!(config.term.as_deref(), Some("xterm-256color"));
    }
}

#[cfg(all(test, feature = "pty-capture", unix))]
mod tests {
    use super::*;
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use ftui_runtime::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
    use std::time::{Duration, Instant};

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
        cmd.args(["-c", "printf 'ok \\033[31mred\\033[0m\\n'"]);

        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();
        let mut writer = create_writer();
        {
            let mut sink = LogSink::new(&mut writer);
            let start = std::time::Instant::now();
            let mut drained = 0usize;
            while drained == 0 && start.elapsed() < Duration::from_secs(2) {
                drained = capture.drain_to_log_sink(&mut sink).expect("drain to sink");
                if drained == 0 && !capture.is_eof() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            sink.flush().expect("flush sink");
        }

        let output = writer.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("ok red"));
        assert!(!output_str.contains("\x1b[31m"));
    }

    fn fnv1a_64(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn normalize_output(output: &[u8]) -> String {
        String::from_utf8_lossy(output).replace("\r\n", "\n")
    }

    fn jsonl_line(run_id: &str, seed: u64, checksum: u64, env: &[(&str, &str)]) -> String {
        let mut env_pairs = String::new();
        for (idx, (key, value)) in env.iter().enumerate() {
            if idx > 0 {
                env_pairs.push(',');
            }
            env_pairs.push('"');
            env_pairs.push_str(key);
            env_pairs.push_str("\":\"");
            env_pairs.push_str(value);
            env_pairs.push('"');
        }

        format!(
            "{{\"run_id\":\"{run_id}\",\"seed\":{seed},\"checksum\":{checksum},\"env\":{{{env_pairs}}}}}\n"
        )
    }

    fn drain_until_eof(capture: &mut PtyCapture, deadline: Duration) -> io::Result<Vec<u8>> {
        let start = Instant::now();
        let mut output = Vec::new();
        while start.elapsed() < deadline {
            let chunk = capture.read_available_with_timeout(Duration::from_millis(50))?;
            if !chunk.is_empty() {
                output.extend_from_slice(&chunk);
            } else if capture.is_eof() {
                break;
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(output)
    }

    #[test]
    fn pty_capture_timeout_boundary_returns_empty_until_output() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "sleep 0.15; printf late-output"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let early = capture
            .read_available_with_timeout(Duration::from_millis(20))
            .unwrap();
        assert!(early.is_empty(), "expected no output before child writes");

        let later = capture
            .read_available_with_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(
            later
                .windows(b"late-output".len())
                .any(|w| w == b"late-output")
        );
    }

    #[test]
    fn pty_capture_partial_reads_across_calls() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf part-1; sleep 0.2; printf part-2"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let first = capture
            .read_available_with_timeout(Duration::from_millis(50))
            .unwrap();
        assert!(!first.is_empty(), "expected first chunk");

        std::thread::sleep(Duration::from_millis(250));
        let second = capture
            .read_available_with_timeout(Duration::from_millis(200))
            .unwrap();
        assert!(!second.is_empty(), "expected second chunk");

        let mut combined = Vec::new();
        combined.extend_from_slice(&first);
        combined.extend_from_slice(&second);

        assert!(combined.windows(b"part-1".len()).any(|w| w == b"part-1"));
        assert!(combined.windows(b"part-2".len()).any(|w| w == b"part-2"));
    }

    #[test]
    fn pty_capture_deterministic_checksum() {
        let run_id = "pty-capture-test-1";
        let seed = 4242u64;
        let seed_str = seed.to_string();
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf \"run=$FTUI_RUN_ID\\nseed=$FTUI_SEED\\n\""]);
        let config = PtyCaptureConfig::default()
            .with_env("FTUI_RUN_ID", run_id)
            .with_env("FTUI_SEED", seed_str.clone());
        let mut capture = PtyCapture::spawn(config, cmd).unwrap();

        let output = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        let normalized = normalize_output(&output);
        let expected_payload = format!("run={run_id}\nseed={seed}\n");
        assert!(
            normalized.contains(&expected_payload),
            "expected payload in output"
        );

        let checksum = fnv1a_64(normalized.as_bytes());
        let expected_checksum = fnv1a_64(expected_payload.as_bytes());
        assert_eq!(checksum, expected_checksum);

        let log_line = jsonl_line(
            run_id,
            seed,
            checksum,
            &[
                ("TERM", "xterm-256color"),
                ("FTUI_RUN_ID", run_id),
                ("FTUI_SEED", seed_str.as_str()),
            ],
        );
        assert!(log_line.contains("\"run_id\":\"pty-capture-test-1\""));
        assert!(log_line.contains("\"seed\":4242"));
        assert!(log_line.contains("\"env\""));
        assert!(log_line.contains("\"FTUI_RUN_ID\""));
        assert!(log_line.contains("\"FTUI_SEED\""));
    }

    #[test]
    fn pty_capture_send_input() {
        // cat reads stdin and echoes to stdout
        let cmd = CommandBuilder::new("cat");
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        capture.send_input(b"hello\n").unwrap();
        let output = drain_until_eof(&mut capture, Duration::from_millis(500)).unwrap();
        let text = normalize_output(&output);
        assert!(text.contains("hello"), "expected echo of input");
    }

    #[test]
    fn pty_capture_send_empty_input_is_noop() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf done"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        // Empty send should succeed without error
        capture.send_input(b"").unwrap();

        let output = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn pty_capture_wait_returns_exit_status() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "exit 0"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let status = capture.wait().unwrap();
        assert!(status.success());
    }

    #[test]
    fn pty_capture_wait_nonzero_exit() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "exit 42"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let status = capture.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn pty_capture_child_pid_is_some() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "exit 0"]);
        let capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        // On unix, process_id should return Some
        assert!(capture.child_pid().is_some());
    }

    #[test]
    fn pty_capture_is_eof_initially_false() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "sleep 0.5"]);
        let capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        assert!(!capture.is_eof());
    }

    #[test]
    fn pty_capture_eof_after_child_exits() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf eof-test"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let _ = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        // After draining to EOF, is_eof should be true
        assert!(capture.is_eof());
    }

    #[test]
    fn pty_capture_read_available_nonblocking() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "sleep 1; printf never"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        // Non-blocking read should return empty immediately
        let output = capture.read_available().unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn pty_capture_read_after_eof_returns_empty() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf done"]);
        let mut capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let _ = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        assert!(capture.is_eof());

        // Subsequent reads should return empty
        let output = capture.read_available().unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn pty_capture_debug_format() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "exit 0"]);
        let capture = PtyCapture::spawn(PtyCaptureConfig::default(), cmd).unwrap();

        let dbg = format!("{capture:?}");
        assert!(dbg.contains("PtyCapture"));
        assert!(dbg.contains("eof"));
    }

    #[test]
    fn pty_capture_custom_size() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "tput cols; tput lines"]);
        let config = PtyCaptureConfig::default().with_size(132, 50);
        let mut capture = PtyCapture::spawn(config, cmd).unwrap();

        let output = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        let text = normalize_output(&output);
        assert!(text.contains("132"), "expected cols=132 in output");
        assert!(text.contains("50"), "expected lines=50 in output");
    }

    #[test]
    fn pty_capture_custom_env() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf $MY_VAR"]);
        let config = PtyCaptureConfig::default().with_env("MY_VAR", "custom_value");
        let mut capture = PtyCapture::spawn(config, cmd).unwrap();

        let output = drain_until_eof(&mut capture, Duration::from_secs(2)).unwrap();
        let text = normalize_output(&output);
        assert!(text.contains("custom_value"), "expected env var in output");
    }
}
