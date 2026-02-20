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
//! channel. The caller is responsible for polling [`RenderThread::check_error`]
//! to detect failures.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use web_time::Instant;

use crate::terminal_writer::{ScreenMode, TerminalWriter};
use ftui_render::buffer::Buffer;

/// Maximum number of log messages processed per render-loop iteration.
///
/// This prevents log spam from indefinitely starving UI presents.
const LOG_CHUNK_LIMIT: usize = 64;

/// Channel capacity for the outbound message queue.
const CHANNEL_CAPACITY: usize = 256;

/// Pending render state: buffer, cursor position, cursor visibility.
type PendingRender = (Buffer, Option<(u16, u16)>, bool);

/// Messages sent from the main thread to the render thread.
#[derive(Debug)]
pub enum OutMsg {
    Log(Vec<u8>),
    Render {
        buffer: Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    },
    Resize {
        w: u16,
        h: u16,
    },
    SetMode(ScreenMode),
    Shutdown,
}

pub struct RenderThread {
    sender: mpsc::SyncSender<OutMsg>,
    handle: Option<JoinHandle<()>>,
    error_rx: mpsc::Receiver<io::Error>,
}

#[allow(clippy::result_large_err)]
impl RenderThread {
    pub fn start<W: Write + Send + 'static>(writer: TerminalWriter<W>) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<OutMsg>(CHANNEL_CAPACITY);
        let (err_tx, err_rx) = mpsc::sync_channel::<io::Error>(8);

        let handle = thread::Builder::new()
            .name("ftui-render".into())
            .spawn(move || {
                render_loop(writer, rx, err_tx);
            })?;

        Ok(Self {
            sender: tx,
            handle: Some(handle),
            error_rx: err_rx,
        })
    }

    pub fn send(&self, msg: OutMsg) -> Result<(), mpsc::SendError<OutMsg>> {
        self.sender.send(msg)
    }

    pub fn try_send(&self, msg: OutMsg) -> Result<(), mpsc::TrySendError<OutMsg>> {
        self.sender.try_send(msg)
    }

    pub fn check_error(&self) -> Option<io::Error> {
        self.error_rx.try_recv().ok()
    }

    pub fn shutdown(mut self) {
        let _ = self.sender.send(OutMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        let _ = self.sender.send(OutMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn render_loop<W: Write + Send>(
    mut writer: TerminalWriter<W>,
    rx: mpsc::Receiver<OutMsg>,
    err_tx: mpsc::SyncSender<io::Error>,
) {
    let mut loop_count: u64 = 0;
    // Reuse buffer to avoid allocation churn in the hot loop
    let mut logs: Vec<Vec<u8>> = Vec::with_capacity(LOG_CHUNK_LIMIT * 4);
    let mut last_render_time = Instant::now();

    loop {
        loop_count += 1;
        let first = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => return,
        };

        logs.clear();
        let mut latest_render: Option<PendingRender> = None;
        let mut shutdown = false;

        process_msg(
            first,
            &mut logs,
            &mut latest_render,
            &mut writer,
            &mut shutdown,
            &err_tx,
        );

        if !shutdown {
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

        // Processing Logic:
        // 1. If we have logs, write them in chunks.
        // 2. After each chunk, if we have a render, present it.
        //    This ensures the UI is updated periodically during heavy logging.
        // 3. If we have no logs but have a render, present it.

        if logs.is_empty() {
            if let Some((buffer, cursor, cursor_visible)) = &latest_render {
                if let Err(e) = writer.present_ui(buffer, *cursor, *cursor_visible) {
                    let _ = err_tx.try_send(e);
                    return;
                }
                last_render_time = Instant::now();
            }
        } else {
            let chunks_len = logs.chunks(LOG_CHUNK_LIMIT).len();
            for (i, chunk) in logs.chunks(LOG_CHUNK_LIMIT).enumerate() {
                let is_last_chunk = i == chunks_len - 1;

                // Write chunk
                for log_bytes in chunk {
                    if let Err(e) = writer.write_log(&String::from_utf8_lossy(log_bytes)) {
                        let _ = err_tx.try_send(e);
                        return;
                    }
                }

                // Interleaved render (throttled)
                // Only render if sufficient time has passed (e.g. 33ms for ~30fps) to avoid
                // thrashing the terminal with Clear/Draw cycles when logs invalidate the UI state.
                // Always render on the last chunk to ensure final consistency.
                let now = Instant::now();
                let should_render =
                    is_last_chunk || now.duration_since(last_render_time).as_millis() >= 33;

                if should_render
                    && let Some((buffer, cursor, cursor_visible)) = &latest_render
                {
                    if let Err(e) = writer.present_ui(buffer, *cursor, *cursor_visible) {
                        let _ = err_tx.try_send(e);
                        return;
                    }
                    last_render_time = now;
                }
            }
        }

        // Periodic grapheme pool GC
        if loop_count.is_multiple_of(1000) {
            writer.gc(latest_render.as_ref().map(|(buf, _, _)| buf));
        }

        if shutdown {
            let _ = writer.flush();
            return;
        }
    }
}

fn process_msg<W: Write>(
    msg: OutMsg,
    logs: &mut Vec<Vec<u8>>,
    latest_render: &mut Option<PendingRender>,
    writer: &mut TerminalWriter<W>,
    shutdown: &mut bool,
    _err_tx: &mpsc::SyncSender<io::Error>,
) {
    match msg {
        OutMsg::Log(bytes) => {
            logs.push(bytes);
        }
        OutMsg::Render {
            buffer,
            cursor,
            cursor_visible,
        } => {
            *latest_render = Some((buffer, cursor, cursor_visible));
        }
        OutMsg::Resize { w, h } => {
            writer.set_size(w, h);
        }
        OutMsg::SetMode(_mode) => {
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
            self.inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .write(buf)
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
        let rt = RenderThread::start(writer).unwrap();
        rt.shutdown();
    }

    #[test]
    fn send_log_is_written() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();

        rt.send(OutMsg::Log(b"hello world\n".to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);
        assert!(output.contains("hello world"));
    }

    #[test]
    fn interleaved_logs_and_renders() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 10);
        let rt = RenderThread::start(writer).unwrap();

        // Send enough logs to force chunking (> 64)
        // plus a render in between
        let mut logs = Vec::new();
        for i in 0..100 {
            logs.push(OutMsg::Log(format!("log-{i}\n").into_bytes()));
        }

        let mut buf = Buffer::new(10, 5);
        buf.set_raw(0, 0, Cell::from_char('X'));

        // Send logs, then render
        // Note: channel is FIFO. render loop drains ALL.
        // It will see 100 logs + 1 render (if we send render last).
        // Since we want to test interleaving logic, we rely on the loop's
        // behavior of processing the batch.

        for msg in logs {
            rt.send(msg).unwrap();
        }
        rt.send(OutMsg::Render {
            buffer: buf,
            cursor: None,
            cursor_visible: true,
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(200));
        rt.shutdown();

        let raw = tw.output();
        let output = String::from_utf8_lossy(&raw);

        // Verify logs are present
        assert!(output.contains("log-0"));
        assert!(output.contains("log-99"));

        // Verify render occurred (X is present)
        // With corrected logic, X should appear after first chunk(64),
        // and potentially again after last chunk (100-64=36).
        assert!(output.contains('X'));
    }

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(LOG_CHUNK_LIMIT, 64);
        assert_eq!(CHANNEL_CAPACITY, 256);
    }

    #[test]
    fn check_error_none_when_clean() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }

    #[test]
    fn try_send_succeeds() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();

        assert!(
            rt.try_send(OutMsg::Log(
                b"try-send-test
"
                .to_vec()
            ))
            .is_ok()
        );
        std::thread::sleep(Duration::from_millis(50));
        rt.shutdown();

        let bytes = tw.output();
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("try-send-test"));
    }

    #[test]
    fn drop_triggers_shutdown() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();
        // Drop without explicit shutdown should not hang
        drop(rt);
    }

    #[test]
    fn render_coalescing_uses_latest() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 5);
        let rt = RenderThread::start(writer).unwrap();

        // Send two renders in quick succession: first with 'A', second with 'Z'
        let mut buf_a = Buffer::new(10, 5);
        buf_a.set_raw(0, 0, Cell::from_char('A'));
        let mut buf_z = Buffer::new(10, 5);
        buf_z.set_raw(0, 0, Cell::from_char('Z'));

        rt.send(OutMsg::Render {
            buffer: buf_a,
            cursor: None,
            cursor_visible: true,
        })
        .unwrap();
        rt.send(OutMsg::Render {
            buffer: buf_z,
            cursor: None,
            cursor_visible: true,
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(100));
        rt.shutdown();

        let bytes = tw.output();
        let output = String::from_utf8_lossy(&bytes);
        // The latest render (Z) must appear
        assert!(output.contains('Z'));
    }

    #[test]
    fn resize_message_processed() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();
        // Resize should not cause errors
        rt.send(OutMsg::Resize { w: 120, h: 40 }).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }

    #[test]
    fn set_mode_message_processed() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();
        rt.send(OutMsg::SetMode(ScreenMode::AltScreen)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }

    #[test]
    fn multiple_logs_all_written() {
        let (writer, tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();

        for i in 0..10 {
            rt.send(OutMsg::Log(
                format!(
                    "msg-{i}
"
                )
                .into_bytes(),
            ))
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        rt.shutdown();

        let bytes = tw.output();
        let output = String::from_utf8_lossy(&bytes);
        for i in 0..10 {
            assert!(output.contains(&format!("msg-{i}")), "missing msg-{i}");
        }
    }

    #[test]
    fn outmsg_debug_format() {
        let log = OutMsg::Log(b"hi".to_vec());
        let dbg = format!("{log:?}");
        assert!(dbg.contains("Log"));

        let render = OutMsg::Render {
            buffer: Buffer::new(1, 1),
            cursor: Some((0, 0)),
            cursor_visible: false,
        };
        let dbg = format!("{render:?}");
        assert!(dbg.contains("Render"));

        let resize = OutMsg::Resize { w: 80, h: 24 };
        let dbg = format!("{resize:?}");
        assert!(dbg.contains("Resize"));

        let shutdown = OutMsg::Shutdown;
        let dbg = format!("{shutdown:?}");
        assert!(dbg.contains("Shutdown"));
    }

    #[test]
    fn send_after_shutdown_returns_err() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();
        let _ = rt.sender.send(OutMsg::Shutdown);
        // Wait for render thread to exit
        std::thread::sleep(Duration::from_millis(100));
        // Now sending should fail (disconnected)
        let result = rt.send(OutMsg::Log(
            b"late
"
            .to_vec(),
        ));
        assert!(result.is_err());
    }

    #[test]
    fn set_mode_debug_format() {
        let msg = OutMsg::SetMode(ScreenMode::AltScreen);
        let dbg = format!("{msg:?}");
        assert!(dbg.contains("SetMode"));
    }

    #[test]
    fn render_with_cursor_position() {
        let (mut writer, tw) = test_writer();
        writer.set_size(10, 5);
        let rt = RenderThread::start(writer).unwrap();

        let buf = Buffer::new(10, 5);
        rt.send(OutMsg::Render {
            buffer: buf,
            cursor: Some((3, 2)),
            cursor_visible: true,
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.check_error().is_none());
        rt.shutdown();

        let bytes = tw.output();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn render_with_hidden_cursor() {
        let (mut writer, _tw) = test_writer();
        writer.set_size(10, 5);
        let rt = RenderThread::start(writer).unwrap();

        let buf = Buffer::new(10, 5);
        rt.send(OutMsg::Render {
            buffer: buf,
            cursor: None,
            cursor_visible: false,
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }

    #[test]
    fn rapid_resize_messages() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();

        for size in [(80, 24), (120, 40), (40, 10), (200, 60)] {
            rt.send(OutMsg::Resize {
                w: size.0,
                h: size.1,
            })
            .unwrap();
        }

        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }

    #[test]
    fn sequential_log_send_no_panic() {
        let (writer, _tw) = test_writer();
        let rt = RenderThread::start(writer).unwrap();

        // Rapidly send different message types in sequence
        rt.send(OutMsg::Log(b"line-1\n".to_vec())).unwrap();
        rt.send(OutMsg::Resize { w: 20, h: 10 }).unwrap();
        rt.send(OutMsg::Log(b"line-2\n".to_vec())).unwrap();
        rt.send(OutMsg::SetMode(ScreenMode::AltScreen)).unwrap();
        rt.send(OutMsg::Log(b"line-3\n".to_vec())).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        assert!(rt.check_error().is_none());
        rt.shutdown();
    }
}
