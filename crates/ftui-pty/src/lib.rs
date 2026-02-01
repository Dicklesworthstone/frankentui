// Note: We allow unsafe code because Rust 2024 requires unsafe for std::env::set_var,
// which we use in the child process after fork (where it's safe since single-threaded).

//! PTY utilities for integration tests.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use ftui_core::terminal_session::{SessionOptions, TerminalSession};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::poll::{PollFd, PollFlags, poll};
#[cfg(unix)]
use nix::pty::{ForkptyResult, Winsize, forkpty};
#[cfg(unix)]
use nix::sys::wait::{WaitStatus, waitpid};
#[cfg(unix)]
use nix::unistd::{ForkResult, Pid, close, read, write};
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(unix)]
use std::process::ExitStatus;

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

    /// Build expectations from the session options used in the child.
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

#[cfg(unix)]
#[derive(Debug)]
pub struct PtySession {
    master_fd: RawFd,
    child_pid: Pid,
    captured: Vec<u8>,
    config: PtyConfig,
    eof: bool,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct PtySession {
    config: PtyConfig,
}

/// Spawn a PTY and run the closure with a `TerminalSession` in the child.
pub fn spawn_app<F>(f: F) -> io::Result<PtySession>
where
    F: FnOnce(&mut TerminalSession) -> io::Result<()>,
{
    spawn_app_with(PtyConfig::default(), SessionOptions::default(), f)
}

/// Spawn a PTY with custom config and session options.
pub fn spawn_app_with<F>(
    config: PtyConfig,
    session_options: SessionOptions,
    f: F,
) -> io::Result<PtySession>
where
    F: FnOnce(&mut TerminalSession) -> io::Result<()>,
{
    spawn_app_with_unix(config, session_options, f)
}

#[cfg(unix)]
fn spawn_app_with_unix<F>(
    mut config: PtyConfig,
    session_options: SessionOptions,
    f: F,
) -> io::Result<PtySession>
where
    F: FnOnce(&mut TerminalSession) -> io::Result<()>,
{
    if let Some(name) = config.test_name.as_ref() {
        log_event(config.log_events, "PTY_TEST_START", name);
    }

    let winsize = Winsize {
        ws_row: config.rows,
        ws_col: config.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: forkpty is safe when the child process doesn't use threads
    // before exec. We control the child code and ensure thread safety.
    let ForkptyResult {
        master,
        fork_result,
    } = unsafe { forkpty(Some(&winsize), None) }.map_err(nix_error)?;

    match fork_result {
        ForkResult::Parent { child } => {
            log_event(
                config.log_events,
                "PTY_SPAWN",
                format!("child_pid={}", child),
            );

            Ok(PtySession {
                master_fd: master,
                child_pid: child,
                captured: Vec::new(),
                config,
                eof: false,
            })
        }
        ForkResult::Child => {
            let _ = close(master);

            // SAFETY: We're in a child process after fork, which is single-threaded.
            // The set_var calls are safe here because there are no other threads.
            if let Some(term) = config.term.take() {
                unsafe { std::env::set_var("TERM", term) };
            }

            for (key, value) in &config.env {
                unsafe { std::env::set_var(key, value) };
            }

            let mut session = TerminalSession::new(session_options)?;
            let result = f(&mut session);
            drop(session);

            match result {
                Ok(()) => std::process::exit(0),
                Err(err) => {
                    log_event(
                        config.log_events,
                        "PTY_CHILD_ERROR",
                        format!("child_error={}", err),
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(not(unix))]
fn spawn_app_with_unix<F>(
    config: PtyConfig,
    _session_options: SessionOptions,
    _f: F,
) -> io::Result<PtySession>
where
    F: FnOnce(&mut TerminalSession) -> io::Result<()>,
{
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "PTY support is not available on this platform. test_name={:?}",
            config.test_name
        ),
    ))
}

#[cfg(unix)]
impl PtySession {
    /// Read any available output without blocking.
    pub fn read_output(&mut self) -> Vec<u8> {
        match self.read_output_result() {
            Ok(output) => output,
            Err(err) => {
                log_event(
                    self.config.log_events,
                    "PTY_READ_ERROR",
                    format!("error={}", err),
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

            let now = Instant::now();
            if now >= deadline {
                break;
            }

            let remaining = deadline - now;
            let _ = self.read_available(remaining)?;

            if self.eof {
                break;
            }
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

        write(self.master_fd, bytes).map_err(nix_error)?;

        log_event(
            self.config.log_events,
            "PTY_INPUT",
            format!("sent_bytes={}", bytes.len()),
        );

        Ok(())
    }

    /// Wait for the child to exit and return its status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        match waitpid(self.child_pid, None).map_err(nix_error)? {
            WaitStatus::Exited(_, code) => Ok(ExitStatus::from_raw(code << 8)),
            WaitStatus::Signaled(_, signal, _) => Ok(ExitStatus::from_raw(signal as i32)),
            other => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected wait status: {:?}", other),
            )),
        }
    }

    /// Access all captured output so far.
    pub fn output(&self) -> &[u8] {
        &self.captured
    }

    /// Child PID for logging/debugging.
    pub fn child_pid(&self) -> Pid {
        self.child_pid
    }

    fn read_available(&mut self, timeout: Duration) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        let mut total = 0usize;
        let mut first = true;
        let mut buffer = [0u8; 8192];

        loop {
            let wait = if first {
                timeout
            } else {
                Duration::from_millis(0)
            };
            let timeout_ms = duration_to_ms(wait);

            let mut fds = [PollFd::new(
                self.master_fd,
                PollFlags::POLLIN | PollFlags::POLLHUP,
            )];
            let ready = poll(&mut fds, timeout_ms).map_err(nix_error)?;
            if ready == 0 {
                break;
            }

            let revents = fds[0].revents().unwrap_or(PollFlags::empty());
            if !revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
                break;
            }

            match read(self.master_fd, &mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    self.captured.extend_from_slice(&buffer[..count]);
                    total = total.saturating_add(count);
                }
                Err(err) => {
                    if err == Errno::EAGAIN || err == Errno::EWOULDBLOCK {
                        break;
                    }
                    return Err(nix_error(err));
                }
            }

            first = false;
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

#[cfg(unix)]
impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = close(self.master_fd);
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

#[cfg(unix)]
fn duration_to_ms(duration: Duration) -> i32 {
    let ms = duration.as_millis();
    if ms > i32::MAX as u128 {
        i32::MAX
    } else {
        ms as i32
    }
}

#[cfg(unix)]
fn nix_error(err: Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
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
    fn spawn_app_captures_output() {
        let config = PtyConfig::default().logging(false);
        let mut session = spawn_app_with(config, SessionOptions::default(), |_term| {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            stdout.write_all(b"hello-pty")?;
            stdout.flush()?;
            Ok(())
        })
        .expect("spawn_app_with should succeed on unix");

        let _ = session.wait().expect("wait should succeed");
        let output = session.read_output();
        assert!(
            output
                .windows(b"hello-pty".len())
                .any(|w| w == b"hello-pty"),
            "expected PTY output to contain test string"
        );
    }
}
