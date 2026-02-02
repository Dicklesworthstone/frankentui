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

use crate::terminal_writer::{ScreenMode, TerminalWriter};
use ftui_render::buffer::Buffer;

/// Maximum number of log messages processed per render-loop iteration.
///
/// This prevents log spam from indefinitely starving UI presents.
const LOG_CHUNK_LIMIT: usize = 64;

/// Channel capacity for the outbound message queue.
const CHANNEL_CAPACITY: usize = 256;

/// Messages sent from the main thread to the render thread.
#[derive(Debug)]
pub enum OutMsg {
    Log(Vec<u8>),
    Render(Buffer),
    Resize { w: u16, h: u16 },
    SetMode(ScreenMode),
    Shutdown,
}

pub struct RenderThread {
    sender: mpsc::SyncSender<OutMsg>,
    handle: Option<JoinHandle<()>>,
    error_rx: mpsc::Receiver<io::Error>,
}

impl RenderThread {
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
    loop {
        let first = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => return,
        };

        let mut logs: Vec<Vec<u8>> = Vec::new();
        let mut latest_render: Option<Buffer> = None;
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
            if let Some(buffer) = &latest_render
                && let Err(e) = writer.present_ui(buffer)
            {
                let _ = err_tx.try_send(e);
                return;
            }
        } else {
            let mut log_iter = logs.into_iter();
            loop {
                // Take a chunk of logs
                let chunk: Vec<_> = log_iter.by_ref().take(LOG_CHUNK_LIMIT).collect();
                if chunk.is_empty() {
                    break;
                }

                // Write chunk
                for log_bytes in chunk {
                    if let Err(e) = writer.write_log(&String::from_utf8_lossy(&log_bytes)) {
                        let _ = err_tx.try_send(e);
                        return;
                    }
                }

                // Interleaved render
                if let Some(buffer) = &latest_render
                    && let Err(e) = writer.present_ui(buffer)
                {
                    let _ = err_tx.try_send(e);
                    return;
                }
            }
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
            *latest_render = Some(buffer);
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
        let rt = RenderThread::start(writer);

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
        rt.send(OutMsg::Render(buf)).unwrap();

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
}
