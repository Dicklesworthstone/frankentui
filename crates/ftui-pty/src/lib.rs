#![forbid(unsafe_code)]

//! PTY utilities for subprocess-based integration tests.
//!
//! # Why this exists
//! FrankenTUI needs PTY-backed tests to validate terminal cleanup behavior and
//! to safely capture subprocess output without corrupting the parent terminal.
//!
//! # Safety / policy
//! - This crate forbids unsafe code (`#![forbid(unsafe_code)]`).
//! - We use `portable-pty` as a safe, cross-platform abstraction.
//!
//! # Modules
//!
//! - [`pty_process`] - Shell process management with `spawn()`, `kill()`, `is_alive()`.
//! - [`virtual_terminal`] - In-memory terminal state machine for testing.
//! - [`input_forwarding`] - Key-to-sequence conversion and paste handling.
//! - [`ws_bridge`] - WebSocket-to-PTY bridge for remote FrankenTerm sessions.
//!
//! # Role in FrankenTUI
//! `ftui-pty` underpins end-to-end and integration tests that need real PTYs.
//! It is used by the harness and test suites to validate behavior that cannot
//! be simulated with pure unit tests.
//!
//! # How it fits in the system
//! This crate does not participate in the runtime or render pipeline directly.
//! Instead, it provides test infrastructure used by `ftui-harness` and E2E
//! scripts to verify correctness and cleanup behavior.

/// Input forwarding: key events to ANSI sequences.
pub mod input_forwarding;

/// PTY process management for shell spawning and lifecycle control.
pub mod pty_process;

/// In-memory virtual terminal state machine for testing.
pub mod virtual_terminal;

/// WebSocket-to-PTY bridge for remote terminal sessions.
pub mod ws_bridge;

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ftui_core::terminal_session::SessionOptions;
use portable_pty::{CommandBuilder, ExitStatus, PtySize};

/// Configuration for PTY-backed test sessions.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// PTY width in columns.
    pub cols: u16,
    /// PTY height in rows.
    pub rows: u16,
    /// TERM to set in the child (defaults to xterm-256color).
    pub term: Option<String>,
    /// Extra environment variables to set in the child.
    pub env: Vec<(String, String)>,
    /// Optional test name for logging context.
    pub test_name: Option<String>,
    /// Enable structured PTY logging to stderr.
    pub log_events: bool,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            term: Some("xterm-256color".to_string()),
            env: Vec::new(),
            test_name: None,
            log_events: true,
        }
    }
}

impl PtyConfig {
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

    /// Attach a test name for logging context.
    #[must_use]
    pub fn with_test_name(mut self, name: impl Into<String>) -> Self {
        self.test_name = Some(name.into());
        self
    }

    /// Enable or disable log output.
    #[must_use]
    pub fn logging(mut self, enabled: bool) -> Self {
        self.log_events = enabled;
        self
    }
}

/// Options for `read_until_with_options`.
#[derive(Debug, Clone)]
pub struct ReadUntilOptions {
    /// Maximum time to wait for the pattern.
    pub timeout: Duration,
    /// Number of retries on transient errors (0 = no retries).
    pub max_retries: u32,
    /// Delay between retries.
    pub retry_delay: Duration,
    /// Minimum bytes to collect before considering a match (0 = no minimum).
    pub min_bytes: usize,
}

impl Default for ReadUntilOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_retries: 0,
            retry_delay: Duration::from_millis(100),
            min_bytes: 0,
        }
    }
}

impl ReadUntilOptions {
    /// Create options with specified timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Default::default()
        }
    }

    /// Set maximum retries on transient errors.
    #[must_use]
    pub fn retries(mut self, count: u32) -> Self {
        self.max_retries = count;
        self
    }

    /// Set delay between retries.
    #[must_use]
    pub fn retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    /// Set minimum bytes to collect before matching.
    #[must_use]
    pub fn min_bytes(mut self, bytes: usize) -> Self {
        self.min_bytes = bytes;
        self
    }
}

/// Expected cleanup sequences after a session ends.
#[derive(Debug, Clone)]
pub struct CleanupExpectations {
    pub sgr_reset: bool,
    pub show_cursor: bool,
    pub alt_screen: bool,
    pub mouse: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub kitty_keyboard: bool,
}

impl CleanupExpectations {
    /// Strict expectations for maximum cleanup validation.
    pub fn strict() -> Self {
        Self {
            sgr_reset: true,
            show_cursor: true,
            alt_screen: true,
            mouse: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        }
    }

    /// Build expectations from the session options used by the child.
    pub fn for_session(options: &SessionOptions) -> Self {
        Self {
            sgr_reset: false,
            show_cursor: true,
            alt_screen: options.alternate_screen,
            mouse: options.mouse_capture,
            bracketed_paste: options.bracketed_paste,
            focus_events: options.focus_events,
            kitty_keyboard: options.kitty_keyboard,
        }
    }
}

#[derive(Debug)]
enum ReaderMsg {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

/// A spawned PTY session with captured output.
pub struct PtySession {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<ReaderMsg>,
    reader_thread: Option<thread::JoinHandle<()>>,
    captured: Vec<u8>,
    eof: bool,
    config: PtyConfig,
}

impl fmt::Debug for PtySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtySession")
            .field("child_pid", &self.child.process_id())
            .field("captured_len", &self.captured.len())
            .field("eof", &self.eof)
            .field("config", &self.config)
            .finish()
    }
}

/// Spawn a command into a new PTY.
///
/// `config.term` and `config.env` are applied to the `CommandBuilder` before spawn.
pub fn spawn_command(mut config: PtyConfig, mut cmd: CommandBuilder) -> io::Result<PtySession> {
    if let Some(name) = config.test_name.as_ref() {
        log_event(config.log_events, "PTY_TEST_START", name);
    }

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

    let (tx, rx) = mpsc::channel::<ReaderMsg>();
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(ReaderMsg::Eof);
                    break;
                }
                Ok(n) => {
                    let _ = tx.send(ReaderMsg::Data(buf[..n].to_vec()));
                }
                Err(err) => {
                    let _ = tx.send(ReaderMsg::Err(err));
                    break;
                }
            }
        }
    });

    Ok(PtySession {
        child,
        writer,
        rx,
        reader_thread: Some(reader_thread),
        captured: Vec::new(),
        eof: false,
        config,
    })
}

impl PtySession {
    /// Read any available output without blocking.
    pub fn read_output(&mut self) -> Vec<u8> {
        match self.read_output_result() {
            Ok(output) => output,
            Err(err) => {
                log_event(
                    self.config.log_events,
                    "PTY_READ_ERROR",
                    format!("error={err}"),
                );
                self.captured.clone()
            }
        }
    }

    /// Read any available output without blocking (fallible).
    pub fn read_output_result(&mut self) -> io::Result<Vec<u8>> {
        let _ = self.read_available(Duration::from_millis(0))?;
        Ok(self.captured.clone())
    }

    /// Read output until a pattern is found or a timeout elapses.
    /// Uses bounded retries for transient read errors.
    pub fn read_until(&mut self, pattern: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        let options = ReadUntilOptions::with_timeout(timeout)
            .retries(3)
            .retry_delay(Duration::from_millis(25));
        self.read_until_with_options(pattern, options)
    }

    /// Read output until a pattern is found, with configurable retry behavior.
    ///
    /// This variant supports:
    /// - Bounded retries on transient errors (e.g., `WouldBlock`, `Interrupted`)
    /// - Minimum bytes threshold before pattern matching
    /// - Configurable retry delay
    ///
    /// # Example
    /// ```ignore
    /// let options = ReadUntilOptions::with_timeout(Duration::from_secs(5))
    ///     .retries(3)
    ///     .retry_delay(Duration::from_millis(50))
    ///     .min_bytes(10);
    /// let output = session.read_until_with_options(b"ready", options)?;
    /// ```
    pub fn read_until_with_options(
        &mut self,
        pattern: &[u8],
        options: ReadUntilOptions,
    ) -> io::Result<Vec<u8>> {
        if pattern.is_empty() {
            return Ok(self.captured.clone());
        }

        let deadline = Instant::now() + options.timeout;
        let mut retries_remaining = options.max_retries;
        let mut last_error: Option<io::Error> = None;

        loop {
            // Check if we have enough bytes and the pattern is found
            if self.captured.len() >= options.min_bytes
                && find_subsequence(&self.captured, pattern).is_some()
            {
                log_event(
                    self.config.log_events,
                    "PTY_CHECK",
                    format!(
                        "pattern_found=0x{} bytes={}",
                        hex_preview(pattern, 16).trim(),
                        self.captured.len()
                    ),
                );
                return Ok(self.captured.clone());
            }

            if self.eof || Instant::now() >= deadline {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read_available(remaining) {
                Ok(_) => {
                    // Reset retry count on successful read
                    retries_remaining = options.max_retries;
                    last_error = None;
                }
                Err(err) if is_transient_error(&err) => {
                    if retries_remaining > 0 {
                        retries_remaining -= 1;
                        log_event(
                            self.config.log_events,
                            "PTY_RETRY",
                            format!(
                                "transient_error={} retries_left={}",
                                err.kind(),
                                retries_remaining
                            ),
                        );
                        std::thread::sleep(options.retry_delay.min(remaining));
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        // Return the last transient error if we exhausted retries, otherwise timeout
        if let Some(err) = last_error {
            return Err(io::Error::new(
                err.kind(),
                format!("PTY read_until failed after retries: {}", err),
            ));
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "PTY read_until timed out (captured {} bytes, need {} + pattern)",
                self.captured.len(),
                options.min_bytes
            ),
        ))
    }

    /// Send input bytes to the child process.
    pub fn send_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        self.writer.write_all(bytes)?;
        self.writer.flush()?;

        log_event(
            self.config.log_events,
            "PTY_INPUT",
            format!("sent_bytes={}", bytes.len()),
        );

        Ok(())
    }

    /// Wait for the child to exit and return its status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Access all captured output so far.
    pub fn output(&self) -> &[u8] {
        &self.captured
    }

    /// Child process id (if available on this platform).
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    fn read_available(&mut self, timeout: Duration) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        let mut total = 0usize;

        // First read: optionally wait up to `timeout`.
        let first = if timeout.is_zero() {
            match self.rx.try_recv() {
                Ok(msg) => Some(msg),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.eof = true;
                    None
                }
            }
        } else {
            match self.rx.recv_timeout(timeout) {
                Ok(msg) => Some(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    None
                }
            }
        };

        let mut msg = match first {
            Some(m) => m,
            None => return Ok(0),
        };

        loop {
            match msg {
                ReaderMsg::Data(bytes) => {
                    total = total.saturating_add(bytes.len());
                    self.captured.extend_from_slice(&bytes);
                }
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

        if total > 0 {
            log_event(
                self.config.log_events,
                "PTY_OUTPUT",
                format!("captured_bytes={}", total),
            );
        }

        Ok(total)
    }

    /// Drain all remaining output until EOF or timeout.
    ///
    /// Call this after `wait()` to ensure all output from the child process
    /// has been captured. This is important because output may still be in
    /// transit through the PTY after the process exits.
    ///
    /// Returns the total number of bytes drained.
    pub fn drain_remaining(&mut self, timeout: Duration) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        let deadline = Instant::now() + timeout;
        let mut total = 0usize;

        log_event(
            self.config.log_events,
            "PTY_DRAIN_START",
            format!("timeout_ms={}", timeout.as_millis()),
        );

        loop {
            if self.eof {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                log_event(
                    self.config.log_events,
                    "PTY_DRAIN_TIMEOUT",
                    format!("captured_bytes={}", total),
                );
                break;
            }

            // Wait for data with remaining timeout
            let msg = match self.rx.recv_timeout(remaining) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    break;
                }
            };

            match msg {
                ReaderMsg::Data(bytes) => {
                    total = total.saturating_add(bytes.len());
                    self.captured.extend_from_slice(&bytes);
                }
                ReaderMsg::Eof => {
                    self.eof = true;
                    break;
                }
                ReaderMsg::Err(err) => return Err(err),
            }

            // Drain any immediately available data without waiting
            loop {
                match self.rx.try_recv() {
                    Ok(ReaderMsg::Data(bytes)) => {
                        total = total.saturating_add(bytes.len());
                        self.captured.extend_from_slice(&bytes);
                    }
                    Ok(ReaderMsg::Eof) => {
                        self.eof = true;
                        break;
                    }
                    Ok(ReaderMsg::Err(err)) => return Err(err),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.eof = true;
                        break;
                    }
                }
            }
        }

        log_event(
            self.config.log_events,
            "PTY_DRAIN_COMPLETE",
            format!("captured_bytes={} eof={}", total, self.eof),
        );

        Ok(total)
    }

    /// Wait for the child and drain all remaining output.
    ///
    /// This is a convenience method that combines `wait()` with `drain_remaining()`.
    /// It ensures deterministic capture by waiting for both the child to exit
    /// AND all output to be received.
    pub fn wait_and_drain(&mut self, drain_timeout: Duration) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        let _ = self.drain_remaining(drain_timeout)?;
        Ok(status)
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Best-effort cleanup: close writer (sends EOF), then try to terminate the child.
        let _ = self.writer.flush();
        let _ = self.child.kill();

        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Assert that terminal cleanup sequences were emitted.
pub fn assert_terminal_restored(
    output: &[u8],
    expectations: &CleanupExpectations,
) -> Result<(), String> {
    let mut failures = Vec::new();

    if expectations.sgr_reset && !contains_any(output, SGR_RESET_SEQS) {
        failures.push("Missing SGR reset (CSI 0 m)");
    }
    if expectations.show_cursor && !contains_any(output, CURSOR_SHOW_SEQS) {
        failures.push("Missing cursor show (CSI ? 25 h)");
    }
    if expectations.alt_screen && !contains_any(output, ALT_SCREEN_EXIT_SEQS) {
        failures.push("Missing alt-screen exit (CSI ? 1049 l)");
    }
    if expectations.mouse && !contains_any(output, MOUSE_DISABLE_SEQS) {
        failures.push("Missing mouse disable (CSI ? 1000... l)");
    }
    if expectations.bracketed_paste && !contains_any(output, BRACKETED_PASTE_DISABLE_SEQS) {
        failures.push("Missing bracketed paste disable (CSI ? 2004 l)");
    }
    if expectations.focus_events && !contains_any(output, FOCUS_DISABLE_SEQS) {
        failures.push("Missing focus disable (CSI ? 1004 l)");
    }
    if expectations.kitty_keyboard && !contains_any(output, KITTY_DISABLE_SEQS) {
        failures.push("Missing kitty keyboard disable (CSI < u)");
    }

    if failures.is_empty() {
        log_event(true, "PTY_TEST_PASS", "terminal cleanup sequences verified");
        return Ok(());
    }

    for failure in &failures {
        log_event(true, "PTY_FAILURE_REASON", *failure);
    }

    log_event(true, "PTY_OUTPUT_DUMP", "hex:");
    for line in hex_dump(output, 4096).lines() {
        log_event(true, "PTY_OUTPUT_DUMP", line);
    }

    log_event(true, "PTY_OUTPUT_DUMP", "printable:");
    for line in printable_dump(output, 4096).lines() {
        log_event(true, "PTY_OUTPUT_DUMP", line);
    }

    Err(failures.join("; "))
}

fn log_event(enabled: bool, event: &str, detail: impl fmt::Display) {
    if !enabled {
        return;
    }

    let timestamp = timestamp_rfc3339();
    eprintln!("[{}] {}: {}", timestamp, event, detail);
}

fn timestamp_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn hex_preview(bytes: &[u8], limit: usize) -> String {
    let mut out = String::new();
    for b in bytes.iter().take(limit) {
        out.push_str(&format!("{:02x}", b));
    }
    if bytes.len() > limit {
        out.push_str("..");
    }
    out
}

fn hex_dump(bytes: &[u8], limit: usize) -> String {
    let mut out = String::new();
    let slice = bytes.get(0..limit).unwrap_or(bytes);

    for (row, chunk) in slice.chunks(16).enumerate() {
        let offset = row * 16;
        out.push_str(&format!("{:04x}: ", offset));
        for b in chunk {
            out.push_str(&format!("{:02x} ", b));
        }
        out.push('\n');
    }

    if bytes.len() > limit {
        out.push_str("... (truncated)\n");
    }

    out
}

fn printable_dump(bytes: &[u8], limit: usize) -> String {
    let mut out = String::new();
    let slice = bytes.get(0..limit).unwrap_or(bytes);

    for (row, chunk) in slice.chunks(16).enumerate() {
        let offset = row * 16;
        out.push_str(&format!("{:04x}: ", offset));
        for b in chunk {
            let ch = if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push('\n');
    }

    if bytes.len() > limit {
        out.push_str("... (truncated)\n");
    }

    out
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn contains_any(haystack: &[u8], needles: &[&[u8]]) -> bool {
    needles
        .iter()
        .any(|needle| find_subsequence(haystack, needle).is_some())
}

fn portable_pty_error<E: fmt::Display>(err: E) -> io::Error {
    io::Error::other(err.to_string())
}

/// Check if an I/O error is transient and worth retrying.
fn is_transient_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
    )
}

const SGR_RESET_SEQS: &[&[u8]] = &[b"\x1b[0m", b"\x1b[m"];
const CURSOR_SHOW_SEQS: &[&[u8]] = &[b"\x1b[?25h"];
const ALT_SCREEN_EXIT_SEQS: &[&[u8]] = &[b"\x1b[?1049l", b"\x1b[?1047l"];
const MOUSE_DISABLE_SEQS: &[&[u8]] = &[
    b"\x1b[?1000;1002;1006l",
    b"\x1b[?1000;1002l",
    b"\x1b[?1000l",
];
const BRACKETED_PASTE_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[?2004l"];
const FOCUS_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[?1004l"];
const KITTY_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[<u"];

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use ftui_core::terminal_session::{TerminalSession, best_effort_cleanup_for_exit};

    #[test]
    fn cleanup_expectations_match_sequences() {
        let output =
            b"\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?1000;1002;1006l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output, &CleanupExpectations::strict())
            .expect("terminal cleanup assertions failed");
    }

    #[test]
    #[should_panic]
    fn cleanup_expectations_fail_when_missing() {
        let output = b"\x1b[?25h";
        assert_terminal_restored(output, &CleanupExpectations::strict())
            .expect("terminal cleanup assertions failed");
    }

    #[cfg(unix)]
    #[test]
    fn spawn_command_captures_output() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf hello-pty"]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        let _status = session.wait().expect("wait should succeed");
        // Use read_until with a timeout to avoid a race condition:
        // after wait() returns, the reader thread may not have drained
        // all PTY output yet. A non-blocking read_output() can miss data.
        let output = session
            .read_until(b"hello-pty", Duration::from_secs(5))
            .expect("expected PTY output to contain test string");
        assert!(
            output
                .windows(b"hello-pty".len())
                .any(|w| w == b"hello-pty"),
            "expected PTY output to contain test string"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_until_with_options_min_bytes() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'short'; sleep 0.05; printf 'longer-output'"]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        // Wait for at least 10 bytes before matching "output"
        let options = ReadUntilOptions::with_timeout(Duration::from_secs(5)).min_bytes(10);

        let output = session
            .read_until_with_options(b"output", options)
            .expect("expected to find pattern with min_bytes");

        assert!(
            output.len() >= 10,
            "expected at least 10 bytes, got {}",
            output.len()
        );
        assert!(
            output.windows(b"output".len()).any(|w| w == b"output"),
            "expected pattern 'output' in captured data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_until_with_options_retries_on_timeout_then_succeeds() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "sleep 0.1; printf done"]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        // Short initial timeout but with retries
        let options = ReadUntilOptions::with_timeout(Duration::from_secs(3))
            .retries(3)
            .retry_delay(Duration::from_millis(50));

        let output = session
            .read_until_with_options(b"done", options)
            .expect("should succeed with retries");

        assert!(
            output.windows(b"done".len()).any(|w| w == b"done"),
            "expected 'done' in output"
        );
    }

    // --- Deterministic capture ordering tests ---

    #[cfg(unix)]
    #[test]
    fn large_output_fully_captured() {
        let config = PtyConfig::default().logging(false);

        // Generate 64KB of output to ensure large buffers are handled
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "dd if=/dev/zero bs=1024 count=64 2>/dev/null | od -v"]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        let _status = session
            .wait_and_drain(Duration::from_secs(5))
            .expect("wait_and_drain");

        // Should have captured substantial output (od output is larger than input)
        let output = session.output();
        assert!(
            output.len() > 50_000,
            "expected >50KB of output, got {} bytes",
            output.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn late_output_after_exit_captured() {
        let config = PtyConfig::default().logging(false);

        // Script that writes output slowly, including after main processing
        let mut cmd = CommandBuilder::new("sh");
        cmd.args([
            "-c",
            "printf 'start\\n'; sleep 0.05; printf 'middle\\n'; sleep 0.05; printf 'end\\n'",
        ]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        // Wait for process to exit
        let _status = session.wait().expect("wait should succeed");

        // Now drain remaining output
        let _drained = session
            .drain_remaining(Duration::from_secs(2))
            .expect("drain_remaining should succeed");

        let output = session.output();
        let output_str = String::from_utf8_lossy(output);

        // Verify all output was captured including late writes
        assert!(
            output_str.contains("start"),
            "missing 'start' in output: {output_str:?}"
        );
        assert!(
            output_str.contains("middle"),
            "missing 'middle' in output: {output_str:?}"
        );
        assert!(
            output_str.contains("end"),
            "missing 'end' in output: {output_str:?}"
        );

        // Verify deterministic ordering (start before middle before end)
        let start_pos = output_str.find("start").unwrap();
        let middle_pos = output_str.find("middle").unwrap();
        let end_pos = output_str.find("end").unwrap();
        assert!(
            start_pos < middle_pos && middle_pos < end_pos,
            "output not in expected order: start={start_pos}, middle={middle_pos}, end={end_pos}"
        );

        // Drain should return 0 on second call (all captured)
        let drained_again = session
            .drain_remaining(Duration::from_millis(100))
            .expect("second drain should succeed");
        assert_eq!(drained_again, 0, "second drain should return 0");
    }

    #[cfg(unix)]
    #[test]
    fn wait_and_drain_captures_all() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args([
            "-c",
            "for i in 1 2 3 4 5; do printf \"line$i\\n\"; sleep 0.02; done",
        ]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        // Use wait_and_drain for deterministic capture
        let status = session
            .wait_and_drain(Duration::from_secs(2))
            .expect("wait_and_drain should succeed");

        assert!(status.success(), "child should succeed");

        let output = session.output();
        let output_str = String::from_utf8_lossy(output);

        // Verify all 5 lines were captured
        for i in 1..=5 {
            assert!(
                output_str.contains(&format!("line{i}")),
                "missing 'line{i}' in output: {output_str:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn wait_and_drain_large_output_ordered() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args([
            "-c",
            "i=1; while [ $i -le 1200 ]; do printf \"line%04d\\n\" $i; i=$((i+1)); done",
        ]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        let status = session
            .wait_and_drain(Duration::from_secs(3))
            .expect("wait_and_drain should succeed");

        assert!(status.success(), "child should succeed");

        let output = session.output();
        let output_str = String::from_utf8_lossy(output);
        let lines: Vec<&str> = output_str.lines().collect();

        assert_eq!(
            lines.len(),
            1200,
            "expected 1200 lines, got {}",
            lines.len()
        );
        assert_eq!(lines.first().copied(), Some("line0001"));
        assert_eq!(lines.last().copied(), Some("line1200"));
    }

    #[cfg(unix)]
    #[test]
    fn drain_remaining_respects_eof() {
        let config = PtyConfig::default().logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'quick'"]);

        let mut session = spawn_command(config, cmd).expect("spawn_command should succeed");

        // Wait for exit and drain
        let _ = session
            .wait_and_drain(Duration::from_secs(2))
            .expect("wait_and_drain");

        // Session should now be at EOF
        assert!(session.eof, "should be at EOF after wait_and_drain");

        // Further drain attempts should return 0 immediately
        let result = session
            .drain_remaining(Duration::from_secs(1))
            .expect("drain");
        assert_eq!(result, 0, "drain after EOF should return 0");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup() {
        let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current exe"));
        cmd.args([
            "--exact",
            "tests::pty_terminal_session_cleanup_child",
            "--nocapture",
        ]);
        cmd.env("FTUI_PTY_CHILD", "1");

        let config = PtyConfig::default()
            .with_test_name("terminal_session_cleanup")
            .logging(false);
        let mut session = spawn_command(config, cmd).expect("spawn PTY child");

        let status = session.wait().expect("wait for child");
        assert!(status.success(), "child test failed: {:?}", status);

        let _ = session
            .read_until(b"\x1b[?25h", Duration::from_secs(5))
            .expect("expected cursor show sequence");
        let _ = session
            .drain_remaining(Duration::from_secs(1))
            .expect("drain remaining");
        let output = session.output();

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(output, &expectations)
            .expect("terminal cleanup assertions failed");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup_child() {
        if std::env::var("FTUI_PTY_CHILD").as_deref() != Ok("1") {
            return;
        }

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };

        let _session = TerminalSession::new(options).expect("TerminalSession::new");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup_on_panic() {
        let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current exe"));
        cmd.args([
            "--exact",
            "tests::pty_terminal_session_cleanup_panic_child",
            "--nocapture",
        ]);
        cmd.env("FTUI_PTY_PANIC_CHILD", "1");

        let config = PtyConfig::default()
            .with_test_name("terminal_session_cleanup_panic")
            .logging(false);
        let mut session = spawn_command(config, cmd).expect("spawn PTY child");

        let status = session.wait().expect("wait for child");
        assert!(
            !status.success(),
            "panic child should exit with failure status"
        );

        let _ = session
            .read_until(b"\x1b[?25h", Duration::from_secs(5))
            .expect("expected cursor show sequence");
        let _ = session
            .drain_remaining(Duration::from_secs(1))
            .expect("drain remaining");
        let output = session.output();

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(output, &expectations)
            .expect("terminal cleanup assertions failed");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup_panic_child() {
        if std::env::var("FTUI_PTY_PANIC_CHILD").as_deref() != Ok("1") {
            return;
        }

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };

        let _session = TerminalSession::new(options).expect("TerminalSession::new");
        std::panic::panic_any("intentional panic to verify cleanup on unwind");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup_on_exit() {
        let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current exe"));
        cmd.args([
            "--exact",
            "tests::pty_terminal_session_cleanup_exit_child",
            "--nocapture",
        ]);
        cmd.env("FTUI_PTY_EXIT_CHILD", "1");

        let config = PtyConfig::default()
            .with_test_name("terminal_session_cleanup_exit")
            .logging(false);
        let mut session = spawn_command(config, cmd).expect("spawn PTY child");

        let status = session.wait().expect("wait for child");
        assert!(status.success(), "exit child should succeed: {:?}", status);

        let _ = session
            .read_until(b"\x1b[?25h", Duration::from_secs(5))
            .expect("expected cursor show sequence");
        let _ = session
            .drain_remaining(Duration::from_secs(1))
            .expect("drain remaining");
        let output = session.output();

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(output, &expectations)
            .expect("terminal cleanup assertions failed");
    }

    #[cfg(unix)]
    #[test]
    fn pty_terminal_session_cleanup_exit_child() {
        if std::env::var("FTUI_PTY_EXIT_CHILD").as_deref() != Ok("1") {
            return;
        }

        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };

        let _session = TerminalSession::new(options).expect("TerminalSession::new");
        best_effort_cleanup_for_exit();
        std::process::exit(0);
    }

    // --- find_subsequence tests ---

    #[test]
    fn find_subsequence_empty_needle() {
        assert_eq!(find_subsequence(b"anything", b""), Some(0));
    }

    #[test]
    fn find_subsequence_empty_haystack() {
        assert_eq!(find_subsequence(b"", b"x"), None);
    }

    #[test]
    fn find_subsequence_found_at_start() {
        assert_eq!(find_subsequence(b"hello world", b"hello"), Some(0));
    }

    #[test]
    fn find_subsequence_found_in_middle() {
        assert_eq!(find_subsequence(b"hello world", b"o w"), Some(4));
    }

    #[test]
    fn find_subsequence_found_at_end() {
        assert_eq!(find_subsequence(b"hello world", b"world"), Some(6));
    }

    #[test]
    fn find_subsequence_not_found() {
        assert_eq!(find_subsequence(b"hello world", b"xyz"), None);
    }

    #[test]
    fn find_subsequence_needle_longer_than_haystack() {
        assert_eq!(find_subsequence(b"ab", b"abcdef"), None);
    }

    #[test]
    fn find_subsequence_exact_match() {
        assert_eq!(find_subsequence(b"abc", b"abc"), Some(0));
    }

    // --- contains_any tests ---

    #[test]
    fn contains_any_finds_first_match() {
        assert!(contains_any(b"\x1b[0m test", &[b"\x1b[0m", b"\x1b[m"]));
    }

    #[test]
    fn contains_any_finds_second_match() {
        assert!(contains_any(b"\x1b[m test", &[b"\x1b[0m", b"\x1b[m"]));
    }

    #[test]
    fn contains_any_no_match() {
        assert!(!contains_any(b"plain text", &[b"\x1b[0m", b"\x1b[m"]));
    }

    #[test]
    fn contains_any_empty_needles() {
        assert!(!contains_any(b"test", &[]));
    }

    // --- hex_preview tests ---

    #[test]
    fn hex_preview_basic() {
        let result = hex_preview(&[0x41, 0x42, 0x43], 10);
        assert_eq!(result, "414243");
    }

    #[test]
    fn hex_preview_truncated() {
        let result = hex_preview(&[0x00, 0x01, 0x02, 0x03, 0x04], 3);
        assert_eq!(result, "000102..");
    }

    #[test]
    fn hex_preview_empty() {
        assert_eq!(hex_preview(&[], 10), "");
    }

    // --- hex_dump tests ---

    #[test]
    fn hex_dump_single_row() {
        let result = hex_dump(&[0x41, 0x42], 100);
        assert!(result.starts_with("0000: "));
        assert!(result.contains("41 42"));
    }

    #[test]
    fn hex_dump_multi_row() {
        let data: Vec<u8> = (0..20).collect();
        let result = hex_dump(&data, 100);
        assert!(result.contains("0000: "));
        assert!(result.contains("0010: ")); // second row at offset 16
    }

    #[test]
    fn hex_dump_truncated() {
        let data: Vec<u8> = (0..100).collect();
        let result = hex_dump(&data, 32);
        assert!(result.contains("(truncated)"));
    }

    #[test]
    fn hex_dump_empty() {
        let result = hex_dump(&[], 100);
        assert!(result.is_empty());
    }

    // --- printable_dump tests ---

    #[test]
    fn printable_dump_ascii() {
        let result = printable_dump(b"Hello", 100);
        assert!(result.contains("Hello"));
    }

    #[test]
    fn printable_dump_replaces_control_chars() {
        let result = printable_dump(&[0x01, 0x02, 0x1B], 100);
        // Control chars should be replaced with '.'
        assert!(result.contains("..."));
    }

    #[test]
    fn printable_dump_truncated() {
        let data: Vec<u8> = (0..100).collect();
        let result = printable_dump(&data, 32);
        assert!(result.contains("(truncated)"));
    }

    // --- PtyConfig builder tests ---

    #[test]
    fn pty_config_defaults() {
        let config = PtyConfig::default();
        assert_eq!(config.cols, 80);
        assert_eq!(config.rows, 24);
        assert_eq!(config.term.as_deref(), Some("xterm-256color"));
        assert!(config.env.is_empty());
        assert!(config.test_name.is_none());
        assert!(config.log_events);
    }

    #[test]
    fn pty_config_with_size() {
        let config = PtyConfig::default().with_size(120, 40);
        assert_eq!(config.cols, 120);
        assert_eq!(config.rows, 40);
    }

    #[test]
    fn pty_config_with_term() {
        let config = PtyConfig::default().with_term("dumb");
        assert_eq!(config.term.as_deref(), Some("dumb"));
    }

    #[test]
    fn pty_config_with_env() {
        let config = PtyConfig::default()
            .with_env("FOO", "bar")
            .with_env("BAZ", "qux");
        assert_eq!(config.env.len(), 2);
        assert_eq!(config.env[0], ("FOO".to_string(), "bar".to_string()));
        assert_eq!(config.env[1], ("BAZ".to_string(), "qux".to_string()));
    }

    #[test]
    fn pty_config_with_test_name() {
        let config = PtyConfig::default().with_test_name("my_test");
        assert_eq!(config.test_name.as_deref(), Some("my_test"));
    }

    #[test]
    fn pty_config_logging_disabled() {
        let config = PtyConfig::default().logging(false);
        assert!(!config.log_events);
    }

    #[test]
    fn pty_config_builder_chaining() {
        let config = PtyConfig::default()
            .with_size(132, 50)
            .with_term("xterm")
            .with_env("KEY", "val")
            .with_test_name("chain_test")
            .logging(false);
        assert_eq!(config.cols, 132);
        assert_eq!(config.rows, 50);
        assert_eq!(config.term.as_deref(), Some("xterm"));
        assert_eq!(config.env.len(), 1);
        assert_eq!(config.test_name.as_deref(), Some("chain_test"));
        assert!(!config.log_events);
    }

    // --- ReadUntilOptions tests ---

    #[test]
    fn read_until_options_defaults() {
        let opts = ReadUntilOptions::default();
        assert_eq!(opts.timeout, Duration::from_secs(5));
        assert_eq!(opts.max_retries, 0);
        assert_eq!(opts.retry_delay, Duration::from_millis(100));
        assert_eq!(opts.min_bytes, 0);
    }

    #[test]
    fn read_until_options_with_timeout() {
        let opts = ReadUntilOptions::with_timeout(Duration::from_secs(10));
        assert_eq!(opts.timeout, Duration::from_secs(10));
        assert_eq!(opts.max_retries, 0); // other fields unchanged
    }

    #[test]
    fn read_until_options_builder_chaining() {
        let opts = ReadUntilOptions::with_timeout(Duration::from_secs(3))
            .retries(5)
            .retry_delay(Duration::from_millis(50))
            .min_bytes(100);
        assert_eq!(opts.timeout, Duration::from_secs(3));
        assert_eq!(opts.max_retries, 5);
        assert_eq!(opts.retry_delay, Duration::from_millis(50));
        assert_eq!(opts.min_bytes, 100);
    }

    // --- is_transient_error tests ---

    #[test]
    fn is_transient_error_would_block() {
        let err = io::Error::new(io::ErrorKind::WouldBlock, "test");
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_interrupted() {
        let err = io::Error::new(io::ErrorKind::Interrupted, "test");
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_timed_out() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "test");
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_not_found() {
        let err = io::Error::new(io::ErrorKind::NotFound, "test");
        assert!(!is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_connection_refused() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "test");
        assert!(!is_transient_error(&err));
    }

    // --- CleanupExpectations tests ---

    #[test]
    fn cleanup_strict_all_true() {
        let strict = CleanupExpectations::strict();
        assert!(strict.sgr_reset);
        assert!(strict.show_cursor);
        assert!(strict.alt_screen);
        assert!(strict.mouse);
        assert!(strict.bracketed_paste);
        assert!(strict.focus_events);
        assert!(strict.kitty_keyboard);
    }

    #[test]
    fn cleanup_for_session_matches_options() {
        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: false,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert!(!expectations.sgr_reset); // always false for for_session
        assert!(expectations.show_cursor); // always true
        assert!(expectations.alt_screen);
        assert!(!expectations.mouse);
        assert!(expectations.bracketed_paste);
        assert!(!expectations.focus_events);
        assert!(expectations.kitty_keyboard);
    }

    #[test]
    fn cleanup_for_session_all_disabled() {
        let options = SessionOptions {
            alternate_screen: false,
            mouse_capture: false,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert!(expectations.show_cursor); // still true
        assert!(!expectations.alt_screen);
        assert!(!expectations.mouse);
        assert!(!expectations.bracketed_paste);
        assert!(!expectations.focus_events);
        assert!(!expectations.kitty_keyboard);
    }

    // --- assert_terminal_restored edge cases ---

    #[test]
    fn assert_restored_with_alt_sequence_variants() {
        // Both alt-screen exit sequences should be accepted
        let output1 = b"\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?1000l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output1, &CleanupExpectations::strict())
            .expect("terminal cleanup assertions failed");

        let output2 = b"\x1b[0m\x1b[?25h\x1b[?1047l\x1b[?1000;1002l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output2, &CleanupExpectations::strict())
            .expect("terminal cleanup assertions failed");
    }

    #[test]
    fn assert_restored_sgr_reset_variant() {
        // Both \x1b[0m and \x1b[m should be accepted for sgr_reset
        let output = b"\x1b[m\x1b[?25h\x1b[?1049l\x1b[?1000l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output, &CleanupExpectations::strict())
            .expect("terminal cleanup assertions failed");
    }

    #[test]
    fn assert_restored_partial_expectations() {
        // Only cursor show required — should pass with just that sequence
        let expectations = CleanupExpectations {
            sgr_reset: false,
            show_cursor: true,
            alt_screen: false,
            mouse: false,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
        };
        assert_terminal_restored(b"\x1b[?25h", &expectations)
            .expect("terminal cleanup assertions failed");
    }

    // --- sequence constant tests ---

    #[test]
    fn sequence_constants_are_nonempty() {
        assert!(!SGR_RESET_SEQS.is_empty());
        assert!(!CURSOR_SHOW_SEQS.is_empty());
        assert!(!ALT_SCREEN_EXIT_SEQS.is_empty());
        assert!(!MOUSE_DISABLE_SEQS.is_empty());
        assert!(!BRACKETED_PASTE_DISABLE_SEQS.is_empty());
        assert!(!FOCUS_DISABLE_SEQS.is_empty());
        assert!(!KITTY_DISABLE_SEQS.is_empty());
    }
}
