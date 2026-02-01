#![forbid(unsafe_code)]

//! Best-effort stdio capture for accidental `println!` protection.
//!
//! When a TUI application runs in inline mode, stray `println!` or `eprintln!`
//! calls from the application or its dependencies write directly to stdout/stderr,
//! corrupting the UI layout. This module provides a channel-based capture system
//! that redirects output through the [`LogSink`](crate::LogSink) → [`TerminalWriter`](crate::TerminalWriter)
//! pipeline, keeping inline mode intact.
//!
//! # How It Works
//!
//! 1. Call [`StdioCapture::install()`] to create a global capture channel.
//! 2. Use the [`ftui_println!`] / [`ftui_eprintln!`] macros instead of `println!` / `eprintln!`.
//! 3. In the event loop, call [`StdioCapture::drain()`] to flush captured bytes
//!    through the terminal writer's log path.
//!
//! # Limitations
//!
//! This is **best-effort** and cannot intercept:
//!
//! - Direct `std::io::stdout().write_all()` calls (no fd-level redirection)
//! - Output from C libraries or FFI code
//! - Writes that bypass Rust's `std::io` entirely
//!
//! For stronger isolation, use PTY-based subprocess capture (see `ftui-extras`
//! crate, `pty_capture` module) which operates at the file-descriptor level.
//!
//! # Thread Safety
//!
//! The capture channel is globally shared behind a `Mutex`. Writers (any thread)
//! acquire the lock briefly to clone the sender, then drop it. The runtime drains
//! the receiver on the main thread during the event loop. Lock contention is
//! minimal because senders are `Send + Clone`.
//!
//! # Example
//!
//! ```rust
//! use ftui_runtime::stdio_capture::StdioCapture;
//!
//! // Install capture (typically done once at program start)
//! let capture = StdioCapture::install().unwrap();
//!
//! // Use the macro instead of println!
//! ftui_runtime::ftui_println!("Hello from captured output");
//!
//! // Drain into any Write sink (normally a LogSink wrapping TerminalWriter)
//! let mut sink = Vec::new();
//! let bytes_drained = capture.drain(&mut sink).unwrap();
//! assert!(bytes_drained > 0);
//! assert!(String::from_utf8_lossy(&sink).contains("Hello from captured output"));
//!
//! // Capture is removed when guard is dropped
//! drop(capture);
//! ```

use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::mpsc;

/// Global sender for the capture channel.
///
/// When `Some`, captured output is routed through the channel.
/// When `None`, macros fall back to regular stdout/stderr.
static CAPTURE_TX: Mutex<Option<mpsc::Sender<Vec<u8>>>> = Mutex::new(None);

/// Error type for stdio capture operations.
#[derive(Debug)]
pub enum StdioCaptureError {
    /// A capture is already installed. Only one can be active at a time.
    AlreadyInstalled,
    /// The internal mutex was poisoned (another thread panicked while holding it).
    PoisonedLock,
}

impl std::fmt::Display for StdioCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "stdio capture is already installed"),
            Self::PoisonedLock => write!(f, "stdio capture lock was poisoned"),
        }
    }
}

impl std::error::Error for StdioCaptureError {}

/// Guard that owns the receiving end of the capture channel.
///
/// While this guard exists, calls to [`ftui_println!`] and [`ftui_eprintln!`]
/// route their output through the capture channel instead of stdout/stderr.
///
/// Call [`drain()`](Self::drain) periodically (e.g., each event-loop iteration)
/// to forward captured bytes to the terminal writer's log path.
///
/// When dropped, the global sender is removed and macros fall back to
/// regular stdout/stderr.
pub struct StdioCapture {
    rx: mpsc::Receiver<Vec<u8>>,
}

impl std::fmt::Debug for StdioCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioCapture")
            .field("installed", &true)
            .finish()
    }
}

impl StdioCapture {
    /// Install the global stdio capture.
    ///
    /// Only one capture can be active at a time. Returns an error if a capture
    /// is already installed.
    ///
    /// # Errors
    ///
    /// - [`StdioCaptureError::AlreadyInstalled`] if called twice without dropping.
    /// - [`StdioCaptureError::PoisonedLock`] if the internal lock is poisoned.
    pub fn install() -> Result<Self, StdioCaptureError> {
        let mut guard = CAPTURE_TX
            .lock()
            .map_err(|_| StdioCaptureError::PoisonedLock)?;

        if guard.is_some() {
            return Err(StdioCaptureError::AlreadyInstalled);
        }

        let (tx, rx) = mpsc::channel();
        *guard = Some(tx);

        Ok(Self { rx })
    }

    /// Check whether a capture is currently installed.
    pub fn is_installed() -> bool {
        CAPTURE_TX.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Drain all pending captured output into the given sink.
    ///
    /// Returns the total number of bytes written. This is non-blocking: it
    /// processes all messages currently in the channel and returns immediately.
    ///
    /// Typical usage passes a [`LogSink`](crate::LogSink) wrapping the
    /// [`TerminalWriter`](crate::TerminalWriter), so captured output flows
    /// through sanitization and the one-writer rule.
    pub fn drain<W: Write>(&self, sink: &mut W) -> io::Result<usize> {
        let mut total = 0;
        while let Ok(bytes) = self.rx.try_recv() {
            sink.write_all(&bytes)?;
            total += bytes.len();
        }
        Ok(total)
    }

    /// Drain pending output, returning it as a `String`.
    ///
    /// Useful for testing. Invalid UTF-8 is replaced with U+FFFD.
    pub fn drain_to_string(&self) -> String {
        let mut buf = Vec::new();
        let _ = self.drain(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl Drop for StdioCapture {
    fn drop(&mut self) {
        // Remove the global sender so macros fall back to stdout/stderr.
        if let Ok(mut guard) = CAPTURE_TX.lock() {
            *guard = None;
        }
        // Drain any remaining messages to prevent channel leak.
        while self.rx.try_recv().is_ok() {}
    }
}

/// Try to send bytes through the capture channel.
///
/// Returns `true` if the bytes were captured, `false` if no capture is installed
/// (or the lock is poisoned). Callers should fall back to direct stdout/stderr
/// when this returns `false`.
///
/// This function is designed to be called from the [`ftui_println!`] and
/// [`ftui_eprintln!`] macros.
pub fn try_capture(bytes: &[u8]) -> bool {
    let Ok(guard) = CAPTURE_TX.lock() else {
        return false;
    };
    if let Some(ref tx) = *guard {
        // Best-effort: if the receiver is dropped, we silently discard.
        let _ = tx.send(bytes.to_vec());
        return true;
    }
    false
}

/// A [`Write`] adapter that sends bytes through the capture channel.
///
/// If no capture is installed, writes are silently accepted (bytes discarded).
/// This implements the "black hole" pattern: callers never see errors from
/// the capture infrastructure itself.
pub struct CapturedWriter;

impl Write for CapturedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        try_capture(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Like `println!` but routes output through ftui's stdio capture system.
///
/// If capture is installed, output goes to the capture channel and will be
/// drained by the runtime through [`LogSink`](crate::LogSink) →
/// [`TerminalWriter::write_log()`](crate::TerminalWriter::write_log).
///
/// If capture is **not** installed, falls back to regular `println!`.
///
/// # Example
///
/// ```rust
/// use ftui_runtime::stdio_capture::StdioCapture;
///
/// let capture = StdioCapture::install().unwrap();
/// ftui_runtime::ftui_println!("count = {}", 42);
///
/// let output = capture.drain_to_string();
/// assert!(output.contains("count = 42"));
/// ```
#[macro_export]
macro_rules! ftui_println {
    () => {
        $crate::ftui_println!("")
    };
    ($($arg:tt)*) => {{
        let msg = ::std::format!("{}\n", ::std::format_args!($($arg)*));
        if !$crate::stdio_capture::try_capture(msg.as_bytes()) {
            ::std::print!("{}", msg);
        }
    }};
}

/// Like `eprintln!` but routes output through ftui's stdio capture system.
///
/// Behaves identically to [`ftui_println!`] when capture is installed.
/// Falls back to `eprintln!` (stderr) when capture is not installed.
#[macro_export]
macro_rules! ftui_eprintln {
    () => {
        $crate::ftui_eprintln!("")
    };
    ($($arg:tt)*) => {{
        let msg = ::std::format!("{}\n", ::std::format_args!($($arg)*));
        if !$crate::stdio_capture::try_capture(msg.as_bytes()) {
            ::std::eprint!("{}", msg);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    // StdioCapture uses a process-global Mutex, so tests that install/uninstall
    // must not run concurrently. We serialize them behind a test-only lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: acquire the serialization lock and ensure no leftover capture.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Clean up any leftover capture from a previous panicked test.
        if let Ok(mut g) = CAPTURE_TX.lock() {
            *g = None;
        }
        guard
    }

    #[test]
    fn install_and_drop_lifecycle() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();
        assert!(StdioCapture::is_installed());
        drop(capture);
        assert!(!StdioCapture::is_installed());
    }

    #[test]
    fn double_install_returns_error() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();
        let result = StdioCapture::install();
        assert!(matches!(
            result.unwrap_err(),
            StdioCaptureError::AlreadyInstalled
        ));
        drop(capture);
    }

    #[test]
    fn reinstall_after_drop() {
        let _g = serial();
        {
            let _c = StdioCapture::install().unwrap();
        }
        // Should succeed after previous capture was dropped
        let capture = StdioCapture::install().unwrap();
        assert!(StdioCapture::is_installed());
        drop(capture);
    }

    #[test]
    fn try_capture_without_install_returns_false() {
        let _g = serial();
        assert!(!try_capture(b"hello"));
    }

    #[test]
    fn try_capture_with_install_returns_true() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();
        assert!(try_capture(b"hello"));
        drop(capture);
    }

    #[test]
    fn drain_returns_captured_bytes() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        try_capture(b"hello ");
        try_capture(b"world\n");

        let mut sink = Vec::new();
        let bytes = capture.drain(&mut sink).unwrap();
        assert_eq!(bytes, 12); // "hello " + "world\n"
        assert_eq!(&sink, b"hello world\n");

        drop(capture);
    }

    #[test]
    fn drain_to_string_works() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        try_capture(b"test message\n");

        let output = capture.drain_to_string();
        assert_eq!(output, "test message\n");

        drop(capture);
    }

    #[test]
    fn drain_empty_returns_zero() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        let mut sink = Vec::new();
        let bytes = capture.drain(&mut sink).unwrap();
        assert_eq!(bytes, 0);
        assert!(sink.is_empty());

        drop(capture);
    }

    #[test]
    fn multiple_drains_are_incremental() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        try_capture(b"first\n");
        let s1 = capture.drain_to_string();
        assert_eq!(s1, "first\n");

        try_capture(b"second\n");
        let s2 = capture.drain_to_string();
        assert_eq!(s2, "second\n");

        // Nothing left
        let s3 = capture.drain_to_string();
        assert!(s3.is_empty());

        drop(capture);
    }

    #[test]
    fn captured_writer_implements_write() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        let mut w = CapturedWriter;
        write!(w, "via writer").unwrap();

        let output = capture.drain_to_string();
        assert_eq!(output, "via writer");

        drop(capture);
    }

    #[test]
    fn captured_writer_without_install_is_silent() {
        let _g = serial();
        let mut w = CapturedWriter;
        let result = write!(w, "discarded");
        assert!(result.is_ok()); // Never errors
    }

    #[test]
    fn ftui_println_macro_captures() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        ftui_println!("formatted: {}", 42);

        let output = capture.drain_to_string();
        assert_eq!(output, "formatted: 42\n");

        drop(capture);
    }

    #[test]
    fn ftui_eprintln_macro_captures() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        ftui_eprintln!("error: {}", "oops");

        let output = capture.drain_to_string();
        assert_eq!(output, "error: oops\n");

        drop(capture);
    }

    #[test]
    fn ftui_println_empty() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        ftui_println!();

        let output = capture.drain_to_string();
        assert_eq!(output, "\n");

        drop(capture);
    }

    #[test]
    fn concurrent_writers() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        let handles: Vec<_> = (0..4)
            .map(|i| {
                std::thread::spawn(move || {
                    for j in 0..10 {
                        let msg = format!("thread-{i}-msg-{j}\n");
                        try_capture(msg.as_bytes());
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let output = capture.drain_to_string();

        // All 40 messages should be captured
        let line_count = output.lines().count();
        assert_eq!(
            line_count, 40,
            "Expected 40 lines from 4 threads x 10 messages, got {line_count}"
        );

        // Each thread's messages should be present
        for i in 0..4 {
            for j in 0..10 {
                assert!(
                    output.contains(&format!("thread-{i}-msg-{j}")),
                    "Missing thread-{i}-msg-{j}"
                );
            }
        }

        drop(capture);
    }

    #[test]
    fn drop_cleans_up_remaining_messages() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();
        try_capture(b"orphaned message\n");
        drop(capture); // Should not leak

        // A new install should work cleanly
        let capture2 = StdioCapture::install().unwrap();
        let output = capture2.drain_to_string();
        assert!(
            output.is_empty(),
            "New capture should not see messages from previous"
        );
        drop(capture2);
    }

    #[test]
    fn error_display() {
        // No global state needed for this test
        let e = StdioCaptureError::AlreadyInstalled;
        assert_eq!(e.to_string(), "stdio capture is already installed");

        let e = StdioCaptureError::PoisonedLock;
        assert_eq!(e.to_string(), "stdio capture lock was poisoned");
    }

    #[test]
    fn binary_data_captured() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        let binary = vec![0u8, 1, 2, 255, 254, 253];
        try_capture(&binary);

        let mut sink = Vec::new();
        capture.drain(&mut sink).unwrap();
        assert_eq!(sink, binary);

        drop(capture);
    }

    #[test]
    fn large_message_captured() {
        let _g = serial();
        let capture = StdioCapture::install().unwrap();

        let large = "x".repeat(1_000_000);
        try_capture(large.as_bytes());

        let output = capture.drain_to_string();
        assert_eq!(output.len(), 1_000_000);

        drop(capture);
    }
}
