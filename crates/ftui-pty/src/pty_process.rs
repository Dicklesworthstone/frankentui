//! PTY process management for shell spawning and lifecycle control.
//!
//! `PtyProcess` provides a higher-level abstraction over `PtySession` specifically
//! designed for spawning and managing interactive shell processes.
//!
//! # Invariants
//!
//! 1. **Single ownership**: Each `PtyProcess` owns exactly one child process.
//! 2. **State consistency**: `is_alive()` reflects the actual process state.
//! 3. **Clean termination**: `kill()` and `Drop` ensure no orphan processes.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | Shell not found | Invalid shell path | `spawn()` returns `Err` with details |
//! | Environment error | Invalid env var | Silently ignored (shell may fail) |
//! | Kill failure | Process already dead | `kill()` succeeds (idempotent) |
//! | Timeout on wait | Process hung | Returns timeout error, process may linger |

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{
    DEFAULT_INPUT_WRITE_TIMEOUT, PtyInputWriter, deadline_after, detach_join, normalize_line_input,
};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize};

/// Configuration for spawning a shell process.
#[derive(Debug, Clone)]
pub struct ShellConfig {
    /// Path to the shell executable.
    /// Defaults to `$SHELL` or `/bin/sh` if not set.
    pub shell: Option<PathBuf>,

    /// Arguments to pass to the shell.
    pub args: Vec<String>,

    /// Environment variables to set in the shell.
    pub env: HashMap<String, String>,

    /// Working directory for the shell.
    pub cwd: Option<PathBuf>,

    /// PTY width in columns.
    pub cols: u16,

    /// PTY height in rows.
    pub rows: u16,

    /// TERM environment variable (defaults to "xterm-256color").
    pub term: String,

    /// Enable logging of PTY events.
    pub log_events: bool,

    /// Maximum time to wait for PTY input writes before failing.
    pub input_write_timeout: Duration,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            cols: 80,
            rows: 24,
            term: "xterm-256color".to_string(),
            log_events: false,
            input_write_timeout: DEFAULT_INPUT_WRITE_TIMEOUT,
        }
    }
}

impl ShellConfig {
    /// Create a new configuration with the specified shell.
    #[must_use]
    pub fn with_shell(shell: impl Into<PathBuf>) -> Self {
        Self {
            shell: Some(shell.into()),
            ..Default::default()
        }
    }

    /// Set the PTY dimensions.
    #[must_use]
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Add a shell argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Set an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Inherit all environment variables from the parent process.
    #[must_use]
    pub fn inherit_env(mut self) -> Self {
        for (key, value) in std::env::vars() {
            self.env.entry(key).or_insert(value);
        }
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Set the TERM environment variable.
    #[must_use]
    pub fn term(mut self, term: impl Into<String>) -> Self {
        self.term = term.into();
        self
    }

    /// Enable or disable event logging.
    #[must_use]
    pub fn logging(mut self, enabled: bool) -> Self {
        self.log_events = enabled;
        self
    }

    /// Override the PTY input write timeout.
    #[must_use]
    pub fn input_write_timeout(mut self, timeout: Duration) -> Self {
        self.input_write_timeout = timeout;
        self
    }

    /// Resolve the shell path.
    fn resolve_shell(&self) -> PathBuf {
        if let Some(ref shell) = self.shell {
            return shell.clone();
        }

        if let Some(shell) = preferred_default_shell() {
            return shell;
        }

        // Try $SHELL environment variable
        if let Ok(shell) = std::env::var("SHELL") {
            return PathBuf::from(shell);
        }

        // Fall back to /bin/sh
        PathBuf::from("/bin/sh")
    }
}

/// Internal message type for the reader thread.
#[derive(Debug)]
enum ReaderMsg {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

/// Process state tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    /// Process is running.
    Running,
    /// Process exited normally with the given status code.
    Exited(u32),
    /// Process was terminated by a signal.
    Signaled(String),
    /// Process state is unknown (e.g., after kill attempt).
    Unknown,
}

impl ProcessState {
    /// Returns `true` if the process is still running.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        matches!(self, ProcessState::Running)
    }

    /// Returns the exit code if the process exited normally.
    #[must_use]
    pub fn exit_code(&self) -> Option<u32> {
        match self {
            ProcessState::Exited(code) => Some(*code),
            _ => None,
        }
    }

    /// Returns the terminating signal description, if any.
    #[must_use]
    pub fn signal_name(&self) -> Option<&str> {
        match self {
            ProcessState::Signaled(signal) => Some(signal.as_str()),
            _ => None,
        }
    }
}

/// A managed PTY process for shell interaction.
///
/// # Example
///
/// ```ignore
/// use ftui_pty::pty_process::{PtyProcess, ShellConfig};
/// use std::time::Duration;
///
/// let config = ShellConfig::default()
///     .inherit_env()
///     .size(80, 24);
///
/// let mut proc = PtyProcess::spawn(config)?;
///
/// // Send a command
/// proc.write_line("echo hello")?;
///
/// // Read output
/// let output = proc.read_until(b"hello", Duration::from_secs(5))?;
///
/// // Check if still alive
/// assert!(proc.is_alive());
///
/// // Clean termination
/// proc.kill()?;
/// ```
pub struct PtyProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    input_writer: PtyInputWriter,
    rx: mpsc::Receiver<ReaderMsg>,
    reader_thread: Option<thread::JoinHandle<()>>,
    captured: Vec<u8>,
    eof: bool,
    state: ProcessState,
    config: ShellConfig,
}

impl fmt::Debug for PtyProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyProcess")
            .field("pid", &self.child.process_id())
            .field("state", &self.state)
            .field("captured_len", &self.captured.len())
            .field("eof", &self.eof)
            .finish()
    }
}

impl PtyProcess {
    /// Spawn a new shell process with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The PTY system cannot be initialized
    /// - The shell executable cannot be found
    /// - The shell fails to start
    pub fn spawn(config: ShellConfig) -> io::Result<Self> {
        let shell_path = config.resolve_shell();

        if config.log_events {
            log_event(
                "PTY_PROCESS_SPAWN",
                format!("shell={}", shell_path.display()),
            );
        }

        // Build the command
        let mut cmd = CommandBuilder::new(&shell_path);

        // Add arguments
        for arg in &config.args {
            cmd.arg(arg);
        }

        // Set environment
        cmd.env("TERM", &config.term);
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // Set working directory
        if let Some(ref cwd) = config.cwd {
            cmd.cwd(cwd);
        }

        // Create PTY
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;

        // Spawn the child
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // Set up I/O
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let input_writer = PtyInputWriter::spawn(writer, "ftui-pty-process-writer")?;

        // Start reader thread
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

        if config.log_events {
            log_event(
                "PTY_PROCESS_STARTED",
                format!("pid={:?}", child.process_id()),
            );
        }

        Ok(Self {
            child,
            master: pair.master,
            input_writer,
            rx,
            reader_thread: Some(reader_thread),
            captured: Vec::new(),
            eof: false,
            state: ProcessState::Running,
            config,
        })
    }

    /// Check if the process is still alive.
    ///
    /// This method polls the process state and updates internal tracking.
    #[must_use]
    pub fn is_alive(&mut self) -> bool {
        self.poll_state();
        self.state.is_alive()
    }

    /// Get the current process state.
    #[must_use]
    pub fn state(&mut self) -> ProcessState {
        self.poll_state();
        self.state.clone()
    }

    /// Get the process ID, if available.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Kill the process.
    ///
    /// This method is idempotent - calling it on an already-dead process succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error if the kill signal cannot be sent.
    pub fn kill(&mut self) -> io::Result<()> {
        if !self.state.is_alive() {
            return Ok(());
        }

        if self.config.log_events {
            log_event(
                "PTY_PROCESS_KILL",
                format!("pid={:?}", self.child.process_id()),
            );
        }

        // Attempt to kill
        self.child.kill()?;
        self.state = ProcessState::Unknown;

        // Wait briefly for the process to actually terminate
        match self.wait_timeout(Duration::from_millis(100)) {
            Ok(status) => {
                self.update_state_from_exit(&status);
            }
            Err(_) => {
                // Process may still be terminating
                self.state = ProcessState::Unknown;
            }
        }

        Ok(())
    }

    /// Wait for the process to exit.
    ///
    /// This blocks until the process terminates or the timeout is reached.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait fails or times out.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.update_state_from_exit(&status);
        Ok(status)
    }

    /// Wait for the process to exit with a timeout.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` if the timeout is reached before the process exits.
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = deadline_after(timeout, "PtyProcess wait_timeout")?;

        loop {
            // Try a non-blocking wait
            match self.child.try_wait()? {
                Some(status) => {
                    self.update_state_from_exit(&status);
                    return Ok(status);
                }
                None => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "wait_timeout: process did not exit in time",
                        ));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Send raw input bytes to the process.
    ///
    /// For interactive shell commands, prefer [`Self::write_line`] so Enter is
    /// encoded as carriage return instead of a bare line feed.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let result = self.input_writer.write_with_timeout(
            data,
            self.config.input_write_timeout,
            "ftui-pty-process-write",
            "ftui-pty-process-detached-write",
        );
        if matches!(
            result.as_ref().err().map(io::Error::kind),
            Some(io::ErrorKind::TimedOut)
        ) {
            let _ = self.child.kill();
            self.state = ProcessState::Unknown;
        }
        result?;

        if self.config.log_events {
            log_event("PTY_PROCESS_INPUT", format!("bytes={}", data.len()));
        }

        Ok(())
    }

    /// Send a line of interactive input, normalizing Enter to carriage return.
    pub fn write_line(&mut self, line: impl AsRef<[u8]>) -> io::Result<()> {
        let normalized = normalize_line_input(line.as_ref());
        self.write_all(&normalized)
    }

    /// Read any available output without blocking.
    pub fn read_available(&mut self) -> io::Result<Vec<u8>> {
        self.drain_channel(Duration::ZERO)?;
        Ok(self.captured.clone())
    }

    /// Read output until a pattern is found or timeout.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` if the pattern is not found within the timeout.
    pub fn read_until(&mut self, pattern: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        if pattern.is_empty() {
            return Ok(self.captured.clone());
        }

        let deadline = deadline_after(timeout, "PtyProcess read_until")?;

        loop {
            // Check if pattern is already in captured data
            if find_subsequence(&self.captured, pattern).is_some() {
                return Ok(self.captured.clone());
            }

            if self.eof || Instant::now() >= deadline {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            self.drain_channel(remaining)?;
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "read_until: pattern not found (captured {} bytes)",
                self.captured.len()
            ),
        ))
    }

    /// Drain all remaining output until EOF or timeout.
    pub fn drain(&mut self, timeout: Duration) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        let start_len = self.captured.len();
        let deadline = deadline_after(timeout, "PtyProcess drain")?;

        while !self.eof && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.drain_channel(remaining) {
                Ok(0) if self.eof => break,
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            }
        }

        Ok(self.captured.len() - start_len)
    }

    /// Get all captured output.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.captured
    }

    /// Clear the captured output buffer.
    pub fn clear_output(&mut self) {
        self.captured.clear();
    }

    /// Resize the PTY.
    ///
    /// This issues TIOCSWINSZ on the master file descriptor, which
    /// delivers SIGWINCH to the child process so it picks up the new
    /// dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        if self.config.log_events {
            log_event("PTY_PROCESS_RESIZE", format!("cols={} rows={}", cols, rows));
        }
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))
    }

    // ── Internal Methods ──────────────────────────────────────────────

    fn poll_state(&mut self) {
        if !self.state.is_alive() {
            return;
        }

        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.update_state_from_exit(&status);
            }
            Ok(None) => {
                // Still running
            }
            Err(_) => {
                self.state = ProcessState::Unknown;
            }
        }
    }

    fn update_state_from_exit(&mut self, status: &ExitStatus) {
        if let Some(signal) = status.signal() {
            self.state = ProcessState::Signaled(signal.to_string());
            return;
        }

        self.state = ProcessState::Exited(status.exit_code());
    }

    fn drain_channel(&mut self, timeout: Duration) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        let mut total = 0usize;

        // First receive with timeout
        let first = if timeout.is_zero() {
            match self.rx.try_recv() {
                Ok(msg) => Some(msg),
                Err(mpsc::TryRecvError::Empty) => return Ok(0),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.eof = true;
                    return Ok(0);
                }
            }
        } else {
            match self.rx.recv_timeout(timeout) {
                Ok(msg) => Some(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(0),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    return Ok(0);
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

        if total > 0 && self.config.log_events {
            log_event("PTY_PROCESS_OUTPUT", format!("bytes={}", total));
        }

        Ok(total)
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        // Best-effort cleanup
        let _ = self.child.kill();
        crate::reap_after_kill(&mut *self.child);
        self.input_writer.flush_best_effort();
        self.input_writer
            .detach_thread("ftui-pty-process-detached-writer");

        if let Some(handle) = self.reader_thread.take() {
            detach_reader_join(handle);
        }

        if self.config.log_events {
            log_event(
                "PTY_PROCESS_DROP",
                format!("pid={:?}", self.child.process_id()),
            );
        }
    }
}

fn detach_reader_join(handle: thread::JoinHandle<()>) {
    detach_join(handle, "ftui-pty-process-detached-reader-join");
}

// ── Helper Functions ──────────────────────────────────────────────────

fn preferred_default_shell() -> Option<PathBuf> {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn log_event(event: &str, detail: impl fmt::Display) {
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
    eprintln!("[{}] {}: {}", timestamp, event, detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ShellConfig Tests ─────────────────────────────────────────────

    #[test]
    fn shell_config_defaults() {
        let config = ShellConfig::default();
        assert!(config.shell.is_none());
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
        assert!(config.cwd.is_none());
        assert_eq!(config.cols, 80);
        assert_eq!(config.rows, 24);
        assert_eq!(config.term, "xterm-256color");
        assert!(!config.log_events);
    }

    #[test]
    fn shell_config_with_shell() {
        let config = ShellConfig::with_shell("/bin/bash");
        assert_eq!(config.shell, Some(PathBuf::from("/bin/bash")));
    }

    #[test]
    fn shell_config_builder_chain() {
        let config = ShellConfig::default()
            .size(120, 40)
            .arg("-l")
            .env("FOO", "bar")
            .cwd("/tmp")
            .term("dumb")
            .logging(true);

        assert_eq!(config.cols, 120);
        assert_eq!(config.rows, 40);
        assert_eq!(config.args, vec!["-l"]);
        assert_eq!(config.env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(config.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(config.term, "dumb");
        assert!(config.log_events);
    }

    #[test]
    fn shell_config_resolve_shell_explicit() {
        let config = ShellConfig::with_shell("/bin/zsh");
        assert_eq!(config.resolve_shell(), PathBuf::from("/bin/zsh"));
    }

    #[test]
    fn shell_config_resolve_shell_prefers_bash_when_available() {
        let config = ShellConfig::default();
        let shell = config.resolve_shell();

        if let Some(preferred) = preferred_default_shell() {
            assert_eq!(shell, preferred);
        } else if let Ok(env_shell) = std::env::var("SHELL") {
            assert_eq!(shell, PathBuf::from(env_shell));
        } else {
            assert_eq!(shell, PathBuf::from("/bin/sh"));
        }
    }

    // ── ProcessState Tests ────────────────────────────────────────────

    #[test]
    fn process_state_is_alive() {
        assert!(ProcessState::Running.is_alive());
        assert!(!ProcessState::Exited(0).is_alive());
        assert!(!ProcessState::Signaled("SIGTERM".to_string()).is_alive());
        assert!(!ProcessState::Unknown.is_alive());
    }

    #[test]
    fn process_state_exit_code() {
        assert_eq!(ProcessState::Running.exit_code(), None);
        assert_eq!(ProcessState::Exited(0).exit_code(), Some(0));
        assert_eq!(ProcessState::Exited(7).exit_code(), Some(7));
        assert_eq!(
            ProcessState::Signaled("SIGTERM".to_string()).exit_code(),
            None
        );
        assert_eq!(ProcessState::Unknown.exit_code(), None);
    }

    #[test]
    fn process_state_signal_name() {
        assert_eq!(ProcessState::Running.signal_name(), None);
        assert_eq!(
            ProcessState::Signaled("Terminated".to_string()).signal_name(),
            Some("Terminated")
        );
        assert_eq!(ProcessState::Exited(7).signal_name(), None);
    }

    // ── find_subsequence Tests ────────────────────────────────────────

    #[test]
    fn find_subsequence_empty_needle() {
        assert_eq!(find_subsequence(b"anything", b""), Some(0));
    }

    #[test]
    fn find_subsequence_found() {
        assert_eq!(find_subsequence(b"hello world", b"world"), Some(6));
    }

    #[test]
    fn find_subsequence_not_found() {
        assert_eq!(find_subsequence(b"hello world", b"xyz"), None);
    }

    // ── PtyProcess Integration Tests ──────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn spawn_and_basic_io() {
        let config = ShellConfig::default()
            .logging(false)
            .env("FTUI_BASIC", "hello-pty-process");
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        // Should be alive
        assert!(proc.is_alive());
        assert!(proc.pid().is_some());

        // Send a simple command
        proc.write_line("echo $FTUI_BASIC")
            .expect("write should succeed");

        let output = proc
            .read_until(b"hello-pty-process", Duration::from_secs(5))
            .expect("should find output");
        assert!(
            output
                .windows(b"hello-pty-process".len())
                .any(|w| w == b"hello-pty-process"),
            "expected to find 'hello-pty-process' in output"
        );

        // Kill the process
        proc.kill().expect("kill should succeed");
        assert!(!proc.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn spawn_with_env() {
        let config = ShellConfig::default()
            .logging(false)
            .env("TEST_VAR", "test_value_123");

        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        proc.write_line("echo $TEST_VAR")
            .expect("write should succeed");

        let output = proc
            .read_until(b"test_value_123", Duration::from_secs(5))
            .expect("should find env var in output");

        assert!(
            output
                .windows(b"test_value_123".len())
                .any(|w| w == b"test_value_123"),
            "expected to find env var value in output"
        );

        proc.kill().expect("kill should succeed");
    }

    #[cfg(unix)]
    #[test]
    fn exit_command_terminates() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        proc.write_line("exit 0").expect("write should succeed");

        // Wait for exit
        let status = proc
            .wait_timeout(Duration::from_secs(5))
            .expect("wait should succeed");
        assert!(status.success());
        assert!(!proc.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_exit_preserves_exit_code() {
        let config = ShellConfig::with_shell("/bin/sh")
            .logging(false)
            .arg("-c")
            .arg("exit 7");
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        let status = proc
            .wait_timeout(Duration::from_secs(5))
            .expect("wait should succeed");
        assert!(!status.success());
        assert_eq!(status.exit_code(), 7);
        assert_eq!(proc.state().exit_code(), Some(7));
        assert_eq!(proc.state(), ProcessState::Exited(7));
    }

    #[cfg(unix)]
    #[test]
    fn signal_exit_preserves_signaled_state() {
        let config = ShellConfig::with_shell("/bin/sh")
            .logging(false)
            .arg("-c")
            .arg("kill -KILL $$");
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        let status = proc
            .wait_timeout(Duration::from_secs(5))
            .expect("wait should succeed");
        assert!(!status.success());
        assert!(status.signal().is_some(), "expected signal exit status");

        let state = proc.state();
        assert!(matches!(state, ProcessState::Signaled(_)));
        assert!(state.signal_name().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn kill_is_idempotent() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        proc.kill().expect("first kill should succeed");
        proc.kill().expect("second kill should succeed");
        proc.kill().expect("third kill should succeed");

        assert!(!proc.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn drain_captures_all_output() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        // Generate output and exit
        proc.write_line("for i in 1 2 3 4 5; do echo line$i; done; exit 0")
            .expect("write should succeed");

        // Wait for exit
        let _ = proc.wait_timeout(Duration::from_secs(5));

        // Drain remaining
        let _ = proc.drain(Duration::from_secs(2));

        let output = String::from_utf8_lossy(proc.output());
        for i in 1..=5 {
            assert!(
                output.contains(&format!("line{i}")),
                "missing line{i} in output: {output:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn clear_output_works() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        proc.write_line("echo test").expect("write should succeed");
        let _ = proc
            .read_until(b"test", Duration::from_secs(5))
            .expect("should capture output after sending a line");

        assert!(!proc.output().is_empty());

        proc.clear_output();
        assert!(proc.output().is_empty());

        proc.kill().expect("kill should succeed");
    }

    #[cfg(unix)]
    #[test]
    fn specific_shell_path() {
        let config = ShellConfig::with_shell("/bin/sh").logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        assert!(proc.is_alive());
        proc.kill().expect("kill should succeed");
    }

    #[cfg(unix)]
    #[test]
    fn invalid_shell_fails() {
        let config = ShellConfig::with_shell("/nonexistent/shell").logging(false);
        let result = PtyProcess::spawn(config);

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn drop_does_not_block_when_background_process_keeps_pty_open() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let (done_tx, done_rx) = mpsc::channel();
        let drop_thread = thread::spawn(move || {
            let proc = PtyProcess::spawn(
                ShellConfig::with_shell(shell)
                    .logging(false)
                    .arg("-c")
                    .arg("sleep 1 >/dev/null 2>&1 &"),
            )
            .expect("spawn should succeed");
            drop(proc);
            done_tx.send(()).expect("signal drop completion");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(400)).is_ok(),
            "PtyProcess drop should not wait for background descendants to close the PTY"
        );
        drop_thread.join().expect("drop thread join");
    }

    #[cfg(unix)]
    #[test]
    fn write_all_times_out_when_child_does_not_drain_stdin() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let config = ShellConfig::with_shell(shell)
            .logging(false)
            .input_write_timeout(Duration::from_millis(100))
            .arg("-c")
            .arg("sleep 5");
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        let payload = vec![b'x'; 8 * 1024 * 1024];
        let start = Instant::now();
        let err = proc
            .write_all(&payload)
            .expect_err("write_all should time out when the child never reads stdin");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "write_all should fail promptly instead of hanging"
        );
    }

    /// The `ps` STAT field for `pid`, or empty when no such process exists.
    #[cfg(unix)]
    fn process_stat(pid: u32) -> String {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// Spawn a child running `script`, drop the `PtyProcess`, and return the
    /// child's lingering `ps` STAT - empty when it was reaped.
    ///
    /// Deliberately `/bin/sh` rather than `$SHELL`: these two tests are about
    /// which branch of the teardown runs, and that depends on whether the
    /// child still exists to ignore a signal. zsh exec-optimises away the last
    /// command of a `-c` script, trap and all, so under `$SHELL` both cases
    /// would collapse into the same one and the second test would pass for no
    /// reason.
    #[cfg(unix)]
    fn stat_after_drop(script: &str) -> (u32, String) {
        // The script echoes READY once it has finished setting itself up, and
        // this waits for it before dropping. Without that handshake the test
        // races the shell: a SIGHUP that arrives before `trap` has run kills
        // the child outright, so the "ignores the hangup" case silently
        // becomes the "takes the hangup" case and passes for no reason. That
        // is exactly what it did before the handshake was added.
        let config = ShellConfig::with_shell("/bin/sh")
            .logging(false)
            .arg("-c")
            .arg(format!("{script}; echo READY; sleep 30"));
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");
        let pid = proc.pid().expect("a spawned child has a pid");
        proc.read_until(b"READY", Duration::from_secs(5))
            .expect("child should reach its ready marker");
        assert!(
            !process_stat(pid).is_empty(),
            "the child should be running before the drop"
        );

        drop(proc);

        // A zombie persists until its parent waits for it, and this test
        // process is that parent, so the state holds for as long as the test
        // runs. The poll is only to let the signals land.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stat = process_stat(pid);
        while !stat.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            stat = process_stat(pid);
        }
        (pid, stat)
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_process_reaps_a_child_that_takes_the_hangup() {
        // Nothing in `Drop` waits, and `std::process::Child` has no `Drop`, so
        // this passes only because `portable_pty`'s `ChildKiller::kill` sends
        // SIGHUP and then polls `try_wait` - which reaps. That is an upstream
        // implementation detail this crate leans on; pin it so a version bump
        // that changes it fails here instead of leaking processes.
        let (pid, stat) = stat_after_drop(":");
        assert!(
            stat.is_empty(),
            "child {pid} outlived its PtyProcess as `{stat}`; \
             a `Z` state means it was killed but never reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_process_reaps_a_child_that_ignores_the_hangup() {
        // The other half of that upstream `kill`: when the child is still
        // alive after the SIGHUP grace period it falls through to
        // `std::process::Child::kill`, which signals and does *not* reap. A
        // shell with `trap '' HUP` takes that branch.
        let (pid, stat) = stat_after_drop("trap '' HUP");
        assert!(
            stat.is_empty(),
            "child {pid} outlived its PtyProcess as `{stat}`; \
             a `Z` state means it was killed but never reaped"
        );
    }
}
