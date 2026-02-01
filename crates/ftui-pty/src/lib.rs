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

    /// Attach a test name for logging context.
    pub fn with_test_name(mut self, name: impl Into<String>) -> Self {
        self.test_name = Some(name.into());
        self
    }

    /// Enable or disable log output.
    pub fn logging(mut self, enabled: bool) -> Self {
        self.log_events = enabled;
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
    pub fn read_until(&mut self, pattern: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        if pattern.is_empty() {
            return Ok(self.captured.clone());
        }

        let deadline = Instant::now() + timeout;

        loop {
            if find_subsequence(&self.captured, pattern).is_some() {
                log_event(
                    self.config.log_events,
                    "PTY_CHECK",
                    format!("pattern_found=0x{}", hex_preview(pattern, 16).trim()),
                );
                return Ok(self.captured.clone());
            }

            if self.eof || Instant::now() >= deadline {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = self.read_available(remaining)?;
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "PTY read_until timed out",
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
            self.rx.try_recv().ok()
        } else {
            self.rx.recv_timeout(timeout).ok()
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
pub fn assert_terminal_restored(output: &[u8], expectations: &CleanupExpectations) {
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
        return;
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

    panic!("PTY cleanup assertions failed: {}", failures.join("; "));
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

    #[test]
    fn cleanup_expectations_match_sequences() {
        let output =
            b"\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?1000;1002;1006l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output, &CleanupExpectations::strict());
    }

    #[test]
    #[should_panic]
    fn cleanup_expectations_fail_when_missing() {
        let output = b"\x1b[?25h";
        assert_terminal_restored(output, &CleanupExpectations::strict());
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
        assert_terminal_restored(output1, &CleanupExpectations::strict());

        let output2 = b"\x1b[0m\x1b[?25h\x1b[?1047l\x1b[?1000;1002l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output2, &CleanupExpectations::strict());
    }

    #[test]
    fn assert_restored_sgr_reset_variant() {
        // Both \x1b[0m and \x1b[m should be accepted for sgr_reset
        let output = b"\x1b[m\x1b[?25h\x1b[?1049l\x1b[?1000l\x1b[?2004l\x1b[?1004l\x1b[<u";
        assert_terminal_restored(output, &CleanupExpectations::strict());
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
        assert_terminal_restored(b"\x1b[?25h", &expectations);
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
