#![forbid(unsafe_code)]

//! Optional dedicated render/output thread (Mode B).
//!
//! When the `render-thread` feature is enabled, this module provides a
//! [`RenderThread`] that moves all terminal output onto a dedicated thread.
//! This solves the interleaving problem where background task logs and
//! tick-driven UI updates could collide, breaking inline mode invariants.
//!
//! The render thread enforces the **one-writer rule** by construction:
//! it is the only place bytes reach the terminal.
//!
//! # Coalescing Rules
//!
//! - **Render** messages are coalesced: if multiple buffers arrive before the
//!   thread processes them, only the latest buffer is presented.
//! - **Log** messages are never dropped, but are chunked to avoid starving
//!   the UI (at most [`LOG_CHUNK_LIMIT`] log messages per iteration).
//! - **Resize** and **SetMode** are applied immediately on the render thread.
//!
//! # Error Propagation
//!
//! IO errors from the render thread are sent back via a dedicated error
//! channel. The caller should periodically call [`RenderThread::check_error`]
//! to detect failures and restore terminal state.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::render_thread::{OutMsg, RenderThread};
//! use ftui_runtime::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//! use ftui_render::buffer::Buffer;
//!
//! let writer = TerminalWriter::new(
//!     std::io::stdout(),
//!     ScreenMode::Inline { ui_height: 10 },
//!     UiAnchor::Bottom,
//!     TerminalCapabilities::detect(),
//! );
//!
//! let rt = RenderThread::start(writer);
//!
//! // Send log (never dropped)
//! rt.send(OutMsg::Log(b"starting up\n".to_vec()));
//!
//! // Send UI frame (coalesced if backed up)
//! let buffer = Buffer::new(80, 10);
//! rt.send(OutMsg::Render(buffer));
//!
//! // Clean shutdown
//! rt.shutdown();
//! ```

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use crate::terminal_writer::{ScreenMode, TerminalWriter};
use ftui_render::buffer::Buffer;

/// Maximum number of log messages processed per render-loop iteration.
///
/// This prevents log spam from indefinitely starving UI presents.
const LOG_CHUNK_LIMIT: usize = 64;

/// Channel capacity for the outbound message queue.
///
/// A bounded channel provides backpressure: if the render thread falls behind,
/// senders block rather than accumulating unbounded memory.
const CHANNEL_CAPACITY: usize = 256;

/// Messages sent from the main thread to the render thread.
#[derive(Debug)]
pub enum OutMsg {
    /// Write log bytes to the terminal scrollback region.
    ///
    /// Log messages are never dropped and are processed in order,
    /// but chunked to avoid starving UI presents.
    Log(Vec<u8>),

    /// Present a new UI frame.
    ///
    /// When multiple Render messages queue up, only the latest buffer
    /// is presented (intermediate frames are dropped).
    Render(Buffer),

    /// The terminal was resized.
    Resize {
        /// New terminal width in columns.
        w: u16,
        /// New terminal height in rows.
        h: u16,
    },

    /// Switch screen mode (e.g., Inline ↔ AltScreen).
    SetMode(ScreenMode),

    /// Shut down the render thread gracefully.
    ///
    /// The thread finishes processing any pending log messages,
    /// presents the final buffer (if any), then exits.
    Shutdown,
}

/// Handle to a running render thread.
///
/// All terminal output flows through this handle via [`OutMsg`] messages.
/// The render thread owns the [`TerminalWriter`] and is the sole writer
/// to the terminal, enforcing the one-writer rule by construction.
pub struct RenderThread {
    sender: mpsc::SyncSender<OutMsg>,
    handle: Option<JoinHandle<()>>,
    error_rx: mpsc::Receiver<io::Error>,
}

impl RenderThread {
    /// Spawn the render thread, transferring ownership of the writer.
    ///
    /// Returns a handle that can send [`OutMsg`] messages to the thread.
    /// The writer is moved to the render thread and cannot be accessed
    /// directly after this call.
    pub fn start<W: Write + Send + 'static>(writer: TerminalWriter<W>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<OutMsg>(CHANNEL_CAPACITY);
        let (err_tx, err_rx) = mpsc::sync_channel::<io::Error>(8);

        let handle = thread::Builder::new()
            .name("ftui-render".into())
            .spawn(move || {
                render_loop(writer, rx, err_tx);
            })
            .expect("failed to spawn render thread");

        Self {
            sender: tx,
            handle: Some(handle),
            error_rx: err_rx,
        }
    }

    /// Send a message to the render thread.
    ///
    /// Returns `Ok(())` if the message was enqueued. Returns `Err` if the
    /// render thread has exited (e.g., after a fatal IO error or shutdown).
    pub fn send(&self, msg: OutMsg) -> Result<(), mpsc::SendError<OutMsg>> {
        self.sender.send(msg)
    }

    /// Try to send without blocking. Returns `Err(TrySendError)` if the
    /// channel is full or the render thread has exited.
    pub fn try_send(&self, msg: OutMsg) -> Result<(), mpsc::TrySendError<OutMsg>> {
        self.sender.try_send(msg)
    }

    /// Check if the render thread has reported an IO error.
    ///
    /// This is a non-blocking poll. Call it periodically (e.g., each
    /// iteration of the main event loop) to detect render failures early.
    pub fn check_error(&self) -> Option<io::Error> {
        self.error_rx.try_recv().ok()
    }

    /// Gracefully shut down the render thread.
    ///
    /// Sends `OutMsg::Shutdown`, then joins the thread. Any remaining
    /// log messages are flushed before exit.
    pub fn shutdown(mut self) {
        // Intentionally consume self to prevent further sends.
        // Take the handle before Drop runs so we join exactly once.
        let _ = self.sender.send(OutMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        // Best-effort shutdown if the caller forgot to call shutdown().
        let _ = self.sender.send(OutMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The render thread's main loop.
///
/// Drains all pending messages, coalesces render frames (latest wins),
/// chunks log output, and handles resize/mode changes.
fn render_loop<W: Write + Send>(
    mut writer: TerminalWriter<W>,
    rx: mpsc::Receiver<OutMsg>,
    err_tx: mpsc::SyncSender<io::Error>,
) {
    loop {
        // Block until at least one message arrives.
        let first = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => return, // Sender dropped, exit cleanly.
        };

        // Drain all currently pending messages into a batch.
        let mut logs: Vec<Vec<u8>> = Vec::new();
        let mut latest_render: Option<Buffer> = None;
        let mut shutdown = false;

        // Process the first message, then drain remaining.
        process_msg(
            first,
            &mut logs,
            &mut latest_render,
            &mut writer,
            &mut shutdown,
            &err_tx,
        );

        if !shutdown {
            // Drain all pending messages (non-blocking).
            while let Ok(msg) = rx.try_recv() {
                process_msg(
                    msg,
                    &mut logs,
                    &mut latest_render,
                    &mut writer,
                    &mut shutdown,
                    &err_tx,
                );
                if shutdown {
                    break;
                }
            }
        }

        // Phase 1: Flush log chunks (up to limit, to avoid starving UI).
        let chunk_end = logs.len().min(LOG_CHUNK_LIMIT);
        for log_bytes in logs.drain(..chunk_end) {
            if let Err(e) = writer.write_log(&String::from_utf8_lossy(&log_bytes)) {
                let _ = err_tx.try_send(e);
                return;
            }
        }

        // Phase 2: Present the latest render buffer (if any).
        if let Some(buffer) = latest_render
            && let Err(e) = writer.present_ui(&buffer)
        {
            let _ = err_tx.try_send(e);
            return;
        }

        // Phase 3: Flush remaining logs that exceeded the chunk limit.
        for log_bytes in logs {
            if let Err(e) = writer.write_log(&String::from_utf8_lossy(&log_bytes)) {
                let _ = err_tx.try_send(e);
                return;
            }
        }

        if shutdown {
            // Flush and exit.
            let _ = writer.flush();
            return;
        }
    }
}

/// Classify and accumulate a single message into the batch state.
fn process_msg<W: Write>(
    msg: OutMsg,
    logs: &mut Vec<Vec<u8>>,
    latest_render: &mut Option<Buffer>,
    writer: &mut TerminalWriter<W>,
    shutdown: &mut bool,
    _err_tx: &mpsc::SyncSender<io::Error>,
) {
    match msg {
        OutMsg::Log(bytes) => {
            logs.push(bytes);
        }
        OutMsg::Render(buffer) => {
            // Latest wins: drop any previously queued buffer.
            *latest_render = Some(buffer);
        }
        OutMsg::Resize { w, h } => {
            // Apply immediately so subsequent renders use new dimensions.
            writer.set_size(w, h);
        }
        OutMsg::SetMode(_mode) => {
            // Screen mode changes require reconstructing the writer, which
            // is not yet supported at runtime. Log a warning for now.
            // TODO(bd-3ky.12): Support runtime mode switching once
            // TerminalWriter supports set_mode().
            tracing::warn!("SetMode received but runtime mode switching not yet implemented");
        }
        OutMsg::Shutdown => {
            *shutdown = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use ftui_render::cell::Cell;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A thread-safe in-memory writer for testing.
    #[derive(Clone)]
    struct TestWriter {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl TestWriter {
        fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn output(&self) -> Vec<u8> {
            self.inner.lock().unwrap().clone()
        }
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_writer() -> (TerminalWriter<TestWriter>, TestWriter) {
        let tw = TestWriter::new();
        let writer = TerminalWriter::new(
            tw.clone(),
            ScreenMode::Inline { ui_height: 5 },
            crate::terminal_writer::UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        );
        (writer, tw)
    }

    #[test]
    fn start_and_shutdown() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer);
        rt.shutdown();
    }

    #[test]
    fn send_log_is_written() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer);

        rt.send(OutMsg::Log(b"hello world\n".to_vec())).unwrap();
        // Give the render thread a moment to process.
        std::thread::sleep(Duration::from_millis(50));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        assert!(
            output.contains("hello world"),
            "Log output should contain 'hello world', got: {output}"
        );
    }

    #[test]
    fn send_render_produces_output() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 10);
        let rt = RenderThread::start(writer);

        let mut buf = Buffer::new(10, 5);
        buf.set_raw(0, 0, Cell::from_char('X'));
        rt.send(OutMsg::Render(buf)).unwrap();

        std::thread::sleep(Duration::from_millis(50));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        assert!(
            output.contains('X'),
            "Render output should contain 'X', got: {output}"
        );
    }

    #[test]
    fn render_coalescing_latest_wins() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 10);
        let rt = RenderThread::start(writer);

        // Send multiple render messages rapidly. The render thread should
        // coalesce them so only the last buffer's content appears.
        for ch in ['A', 'B', 'C', 'D'] {
            let mut buf = Buffer::new(10, 5);
            buf.set_raw(0, 0, Cell::from_char(ch));
            rt.send(OutMsg::Render(buf)).unwrap();
        }

        std::thread::sleep(Duration::from_millis(100));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        // The last rendered character should be present. Due to coalescing,
        // some intermediate characters may be absent, but at minimum the
        // final 'D' should appear.
        assert!(
            output.contains('D'),
            "Coalesced render should contain final 'D', got: {output}"
        );
    }

    #[test]
    fn logs_never_dropped() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer);

        // Send many log messages.
        for i in 0..100 {
            let msg = format!("log-{i}\n");
            rt.send(OutMsg::Log(msg.into_bytes())).unwrap();
        }

        std::thread::sleep(Duration::from_millis(200));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        // All 100 log messages should be present.
        for i in 0..100 {
            assert!(
                output.contains(&format!("log-{i}")),
                "Log message log-{i} should be present"
            );
        }
    }

    #[test]
    fn resize_applied_before_render() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer);

        // Resize then render — the render should use the new dimensions.
        rt.send(OutMsg::Resize { w: 20, h: 15 }).unwrap();
        let buf = Buffer::new(20, 10);
        rt.send(OutMsg::Render(buf)).unwrap();

        std::thread::sleep(Duration::from_millis(50));
        rt.shutdown();
        // If resize was not applied, present_ui would still work but
        // use wrong coordinates. The test passes if no panic occurs.
    }

    #[test]
    fn shutdown_flushes_pending_logs() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer);

        rt.send(OutMsg::Log(b"before-shutdown\n".to_vec())).unwrap();
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        assert!(
            output.contains("before-shutdown"),
            "Logs sent before shutdown should be flushed"
        );
    }

    #[test]
    fn drop_triggers_shutdown() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer);
        // Drop without calling shutdown() — should not hang or panic.
        drop(rt);
    }

    #[test]
    fn send_after_shutdown_returns_error() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer);
        let sender = rt.sender.clone();
        rt.shutdown();

        // The render thread has exited; sending should fail.
        // Give a moment for the thread to fully exit.
        std::thread::sleep(Duration::from_millis(50));
        let result = sender.send(OutMsg::Log(b"after shutdown\n".to_vec()));
        assert!(result.is_err(), "Send after shutdown should fail");
    }

    #[test]
    fn interleaved_logs_and_renders() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 10);
        let rt = RenderThread::start(writer);

        // Interleave log and render messages.
        rt.send(OutMsg::Log(b"log-1\n".to_vec())).unwrap();
        let mut buf = Buffer::new(10, 5);
        buf.set_raw(0, 0, Cell::from_char('A'));
        rt.send(OutMsg::Render(buf)).unwrap();
        rt.send(OutMsg::Log(b"log-2\n".to_vec())).unwrap();
        let mut buf2 = Buffer::new(10, 5);
        buf2.set_raw(0, 0, Cell::from_char('B'));
        rt.send(OutMsg::Render(buf2)).unwrap();
        rt.send(OutMsg::Log(b"log-3\n".to_vec())).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        // All logs should be present.
        assert!(output.contains("log-1"));
        assert!(output.contains("log-2"));
        assert!(output.contains("log-3"));
    }

    #[test]
    fn check_error_returns_none_on_success() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer);

        rt.send(OutMsg::Log(b"ok\n".to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        assert!(rt.check_error().is_none());
        rt.shutdown();
    }
}
