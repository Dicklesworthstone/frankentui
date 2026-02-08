#![forbid(unsafe_code)]
#![cfg(all(unix, feature = "crossterm"))]

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ftui_core::event::Event;
use ftui_core::terminal_session::{SessionOptions, TerminalSession};
use portable_pty::{CommandBuilder, PtySize};

const CURSOR_SHOW: &[u8] = b"\x1b[?25h";
const ALT_SCREEN_EXIT: &[u8] = b"\x1b[?1049l";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

enum ReaderMsg {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

struct PtyHarness {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<ReaderMsg>,
    reader_thread: Option<thread::JoinHandle<()>>,
    captured: Vec<u8>,
    eof: bool,
}

impl PtyHarness {
    fn spawn(cmd: CommandBuilder) -> io::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| io::Error::other(err.to_string()))?;

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| io::Error::other(err.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| io::Error::other(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| io::Error::other(err.to_string()))?;

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

        Ok(Self {
            child,
            writer,
            rx,
            reader_thread: Some(reader_thread),
            captured: Vec::new(),
            eof: false,
        })
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn read_until(&mut self, pattern: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.captured.windows(pattern.len()).any(|w| w == pattern) {
                return Ok(self.captured.clone());
            }
            if self.eof {
                return Ok(self.captured.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(self.captured.clone());
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(100));
            match self.rx.recv_timeout(wait) {
                Ok(ReaderMsg::Data(bytes)) => {
                    self.captured.extend_from_slice(&bytes);
                }
                Ok(ReaderMsg::Eof) => {
                    self.eof = true;
                }
                Ok(ReaderMsg::Err(err)) => return Err(err),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                }
            }
        }
    }

    fn wait_and_drain(&mut self, drain_timeout: Duration) -> io::Result<()> {
        let _ = self.child.wait()?;
        let deadline = Instant::now() + drain_timeout;
        while !self.eof && Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(ReaderMsg::Data(bytes)) => self.captured.extend_from_slice(&bytes),
                Ok(ReaderMsg::Eof) => self.eof = true,
                Ok(ReaderMsg::Err(err)) => return Err(err),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                }
            }
        }
        Ok(())
    }

    fn output(&self) -> &[u8] {
        &self.captured
    }
}

impl Drop for PtyHarness {
    fn drop(&mut self) {
        let _ = self.writer.flush();
        let _ = self.child.kill();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn output_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn spawn_cleanup_child(mode: &str, panic_child: bool) -> io::Result<PtyHarness> {
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current exe"));
    let test_name = if panic_child {
        "pty_cleanup_panic_child"
    } else {
        "pty_cleanup_child"
    };
    cmd.args(["--exact", test_name, "--nocapture"]);
    cmd.env("FTUI_PTY_CHILD", "1");
    cmd.env("FTUI_PTY_MODE", mode);
    PtyHarness::spawn(cmd)
}

fn spawn_event_child() -> io::Result<PtyHarness> {
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("current exe"));
    cmd.args(["--exact", "pty_event_parse_child", "--nocapture"]);
    cmd.env("FTUI_PTY_EVENT_CHILD", "1");
    PtyHarness::spawn(cmd)
}

#[test]
fn pty_terminal_session_cleanup_inline() {
    let mut harness = spawn_cleanup_child("inline", false).expect("spawn inline child");
    let _ = harness
        .read_until(b"READY", Duration::from_secs(2))
        .expect("read READY");
    harness
        .wait_and_drain(Duration::from_secs(2))
        .expect("drain output");

    let output = harness.output();
    assert!(output_contains(output, CURSOR_SHOW), "missing cursor show");
    assert!(
        output_contains(output, BRACKETED_PASTE_DISABLE),
        "missing bracketed paste disable"
    );
}

#[test]
fn pty_terminal_session_cleanup_alt() {
    let mut harness = spawn_cleanup_child("alt", false).expect("spawn alt child");
    let _ = harness
        .read_until(b"READY", Duration::from_secs(2))
        .expect("read READY");
    harness
        .wait_and_drain(Duration::from_secs(2))
        .expect("drain output");

    let output = harness.output();
    assert!(output_contains(output, CURSOR_SHOW), "missing cursor show");
    assert!(
        output_contains(output, ALT_SCREEN_EXIT),
        "missing alt-screen exit"
    );
}

#[test]
fn pty_terminal_session_cleanup_on_panic() {
    let mut harness = spawn_cleanup_child("alt", true).expect("spawn panic child");
    let _ = harness
        .read_until(b"PANIC_READY", Duration::from_secs(2))
        .expect("read PANIC_READY");
    let _ = harness.wait_and_drain(Duration::from_secs(2));
    let output = harness.output();
    assert!(
        output_contains(output, ALT_SCREEN_EXIT),
        "panic path missing alt-screen exit"
    );
    assert!(
        output_contains(output, CURSOR_SHOW),
        "panic path missing cursor show"
    );
}

#[test]
fn pty_event_parsing_basic_keys() {
    let mut harness = spawn_event_child().expect("spawn event child");
    let _ = harness
        .read_until(b"READY", Duration::from_secs(2))
        .expect("read READY");

    harness.write_all(b"a").expect("write char");
    harness.write_all(b"\x1b[A").expect("write up arrow");
    harness.write_all(b"\r").expect("write enter");

    let output = harness
        .read_until(b"DONE", Duration::from_secs(2))
        .expect("read DONE");
    let output_str = String::from_utf8_lossy(&output);

    assert!(
        output_str.contains("EVENT key_code=Char('a')"),
        "missing char event: {output_str}"
    );
    assert!(
        output_str.contains("EVENT key_code=Up"),
        "missing up arrow event: {output_str}"
    );
    assert!(
        output_str.contains("EVENT key_code=Enter"),
        "missing enter event: {output_str}"
    );
}

#[test]
fn pty_cleanup_child() {
    if std::env::var("FTUI_PTY_CHILD").as_deref() != Ok("1") {
        return;
    }
    let mode = std::env::var("FTUI_PTY_MODE").unwrap_or_else(|_| "inline".into());
    let options = SessionOptions {
        alternate_screen: mode == "alt",
        mouse_capture: false,
        bracketed_paste: true,
        focus_events: false,
        kitty_keyboard: false,
    };
    let _session = TerminalSession::new(options).expect("TerminalSession::new");
    println!("READY");
    let _ = io::stdout().flush();
}

#[test]
fn pty_cleanup_panic_child() {
    if std::env::var("FTUI_PTY_CHILD").as_deref() != Ok("1") {
        return;
    }
    let mode = std::env::var("FTUI_PTY_MODE").unwrap_or_else(|_| "alt".into());
    let options = SessionOptions {
        alternate_screen: mode == "alt",
        mouse_capture: false,
        bracketed_paste: true,
        focus_events: false,
        kitty_keyboard: false,
    };
    let _session = TerminalSession::new(options).expect("TerminalSession::new");
    println!("PANIC_READY");
    let _ = io::stdout().flush();
    panic!("intentional panic for cleanup verification");
}

#[test]
fn pty_event_parse_child() {
    if std::env::var("FTUI_PTY_EVENT_CHILD").as_deref() != Ok("1") {
        return;
    }
    let options = SessionOptions {
        alternate_screen: false,
        mouse_capture: false,
        bracketed_paste: true,
        focus_events: false,
        kitty_keyboard: false,
    };
    let session = TerminalSession::new(options).expect("TerminalSession::new");
    println!("READY");
    let _ = io::stdout().flush();

    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while events.len() < 3 && Instant::now() < deadline {
        if session
            .poll_event(Duration::from_millis(50))
            .expect("poll_event")
            && let Some(event) = session.read_event().expect("read_event")
        {
            events.push(event);
        }
    }

    for event in &events {
        match event {
            Event::Key(key) => {
                println!(
                    "EVENT key_code={:?} modifiers={:?}",
                    key.code, key.modifiers
                );
            }
            other => println!("EVENT other={other:?}"),
        }
    }
    println!("DONE");
    let _ = io::stdout().flush();
}
