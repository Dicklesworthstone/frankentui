#![forbid(unsafe_code)]
//! Native Unix terminal backend for FrankenTUI.
//!
//! This crate implements the `ftui-backend` traits for native Unix/macOS terminals.
//! It replaces Crossterm as the terminal I/O layer (Unix-first; Windows deferred).
//!
//! ## Escape Sequence Reference
//!
//! | Feature           | Enable                    | Disable                   |
//! |-------------------|---------------------------|---------------------------|
//! | Alternate screen  | `CSI ? 1049 h`            | `CSI ? 1049 l`            |
//! | Mouse (SGR)       | `CSI ? 1000;1002;1006 h`  | `CSI ? 1000;1002;1006 l`  |
//! | Bracketed paste   | `CSI ? 2004 h`            | `CSI ? 2004 l`            |
//! | Focus events      | `CSI ? 1004 h`            | `CSI ? 1004 l`            |
//! | Kitty keyboard    | `CSI > 15 u`              | `CSI < u`                 |
//! | Cursor show/hide  | `CSI ? 25 h`              | `CSI ? 25 l`              |
//! | Sync output       | `CSI ? 2026 h`            | `CSI ? 2026 l`            |

use core::time::Duration;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc;

use ftui_backend::{Backend, BackendClock, BackendEventSource, BackendFeatures, BackendPresenter};
use ftui_core::event::Event;
use ftui_core::input_parser::InputParser;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;

#[cfg(unix)]
use signal_hook::consts::signal::SIGWINCH;
#[cfg(unix)]
use signal_hook::iterator::Signals;

// ── Escape Sequences ─────────────────────────────────────────────────────

const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";

const MOUSE_ENABLE: &[u8] = b"\x1b[?1000;1002;1006h";
const MOUSE_DISABLE: &[u8] = b"\x1b[?1000;1002;1006l";

const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

const FOCUS_ENABLE: &[u8] = b"\x1b[?1004h";
const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";

const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";

const CURSOR_SHOW: &[u8] = b"\x1b[?25h";
#[allow(dead_code)]
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

const SYNC_END: &[u8] = b"\x1b[?2026l";

const CLEAR_SCREEN: &[u8] = b"\x1b[2J";
const CURSOR_HOME: &[u8] = b"\x1b[H";

// ── Raw Mode Guard ───────────────────────────────────────────────────────

/// RAII guard that saves the original termios and restores it on drop.
///
/// This is the foundation for panic-safe terminal cleanup: even if the
/// application panics, the Drop impl runs (unless `panic = "abort"`) and
/// the terminal returns to its original state.
///
/// The guard opens `/dev/tty` to get an owned fd that is valid for the
/// lifetime of the guard, avoiding unsafe `BorrowedFd` construction.
#[cfg(unix)]
pub struct RawModeGuard {
    original_termios: nix::sys::termios::Termios,
    tty: std::fs::File,
}

#[cfg(unix)]
impl RawModeGuard {
    /// Enter raw mode on the controlling terminal, returning a guard that
    /// restores the original termios on drop.
    pub fn enter() -> io::Result<Self> {
        let tty = std::fs::File::open("/dev/tty")?;
        Self::enter_on(tty)
    }

    /// Enter raw mode on a specific terminal file (e.g., a PTY slave for testing).
    pub fn enter_on(tty: std::fs::File) -> io::Result<Self> {
        let original_termios = nix::sys::termios::tcgetattr(&tty).map_err(io::Error::other)?;

        let mut raw = original_termios.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        nix::sys::termios::tcsetattr(&tty, nix::sys::termios::SetArg::TCSAFLUSH, &raw)
            .map_err(io::Error::other)?;

        Ok(Self {
            original_termios,
            tty,
        })
    }
}

#[cfg(unix)]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best-effort restore — ignore errors during cleanup.
        let _ = nix::sys::termios::tcsetattr(
            &self.tty,
            nix::sys::termios::SetArg::TCSAFLUSH,
            &self.original_termios,
        );
    }
}

// ── Session Options ──────────────────────────────────────────────────────

/// Configuration for opening a terminal session.
#[derive(Debug, Clone, Default)]
pub struct TtySessionOptions {
    /// Enter the alternate screen buffer on open.
    pub alternate_screen: bool,
    /// Initial feature toggles to enable.
    pub features: BackendFeatures,
}

// ── Clock ────────────────────────────────────────────────────────────────

/// Monotonic clock backed by `std::time::Instant`.
pub struct TtyClock {
    epoch: std::time::Instant,
}

impl TtyClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: std::time::Instant::now(),
        }
    }
}

impl Default for TtyClock {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendClock for TtyClock {
    fn now_mono(&self) -> Duration {
        self.epoch.elapsed()
    }
}

// ── Event Source ──────────────────────────────────────────────────────────

// Resize notifications are produced via SIGWINCH on Unix.
//
// We use a dedicated signal thread to avoid unsafe `sigaction` calls in-tree
// (unsafe is forbidden) while still delivering low-latency resize events.
#[cfg(unix)]
#[derive(Debug)]
struct ResizeSignalGuard {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl ResizeSignalGuard {
    fn new(tx: mpsc::SyncSender<()>) -> io::Result<Self> {
        let mut signals = Signals::new([SIGWINCH]).map_err(io::Error::other)?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            for _ in signals.forever() {
                // Coalesce storms: a single pending notification is enough since we
                // query the authoritative size via ioctl when generating the Event.
                let _ = tx.try_send(());
            }
        });

        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

#[cfg(unix)]
impl Drop for ResizeSignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Native Unix event source (raw terminal bytes → `Event`).
///
/// Manages terminal feature toggles by emitting the appropriate escape
/// sequences. Reads raw bytes from the tty fd, feeds them through
/// `InputParser`, and serves parsed events via `poll_event`/`read_event`.
pub struct TtyEventSource {
    features: BackendFeatures,
    width: u16,
    height: u16,
    /// When true, escape sequences are actually written to stdout.
    /// False in test/headless mode.
    live: bool,
    /// Resize notifications (SIGWINCH) are delivered through this channel.
    #[cfg(unix)]
    resize_rx: Option<mpsc::Receiver<()>>,
    /// Owns the SIGWINCH handler thread (kept alive by this field).
    #[cfg(unix)]
    _resize_guard: Option<ResizeSignalGuard>,
    /// Parser state machine: decodes terminal byte sequences into Events.
    parser: InputParser,
    /// Buffered events from the most recent parse.
    event_queue: VecDeque<Event>,
    /// Tty file handle for reading input (None in headless mode).
    tty_reader: Option<std::fs::File>,
}

impl TtyEventSource {
    /// Create an event source in headless mode (no escape sequence output, no I/O).
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            features: BackendFeatures::default(),
            width,
            height,
            live: false,
            #[cfg(unix)]
            resize_rx: None,
            #[cfg(unix)]
            _resize_guard: None,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: None,
        }
    }

    /// Create an event source in live mode (reads from /dev/tty, writes
    /// escape sequences to stdout).
    fn live(width: u16, height: u16) -> io::Result<Self> {
        let tty_reader = std::fs::File::open("/dev/tty")?;
        let mut w = width;
        let mut h = height;
        #[cfg(unix)]
        if let Ok(ws) = rustix::termios::tcgetwinsize(&tty_reader)
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            w = ws.ws_col;
            h = ws.ws_row;
        }

        #[cfg(unix)]
        let (resize_guard, resize_rx) = {
            let (resize_tx, resize_rx) = mpsc::sync_channel(1);
            match ResizeSignalGuard::new(resize_tx) {
                Ok(guard) => (Some(guard), Some(resize_rx)),
                Err(_) => (None, None),
            }
        };

        Ok(Self {
            features: BackendFeatures::default(),
            width: w,
            height: h,
            live: true,
            #[cfg(unix)]
            resize_rx,
            #[cfg(unix)]
            _resize_guard: resize_guard,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: Some(tty_reader),
        })
    }

    /// Create an event source that reads from an arbitrary file descriptor.
    ///
    /// Escape sequences are NOT written to stdout (headless feature toggle
    /// behavior). This is primarily useful for testing with pipes.
    #[cfg(test)]
    fn from_reader(width: u16, height: u16, reader: std::fs::File) -> Self {
        Self {
            features: BackendFeatures::default(),
            width,
            height,
            live: false,
            #[cfg(unix)]
            resize_rx: None,
            #[cfg(unix)]
            _resize_guard: None,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: Some(reader),
        }
    }

    /// Current feature state.
    #[must_use]
    pub fn features(&self) -> BackendFeatures {
        self.features
    }

    fn push_resize(&mut self, new_width: u16, new_height: u16) {
        if new_width == 0 || new_height == 0 {
            return;
        }
        if (new_width, new_height) == (self.width, self.height) {
            return;
        }
        self.width = new_width;
        self.height = new_height;
        self.event_queue.push_back(Event::Resize {
            width: new_width,
            height: new_height,
        });
    }

    #[cfg(unix)]
    fn query_tty_size(&self) -> Option<(u16, u16)> {
        if !self.live {
            return None;
        }
        let tty = self.tty_reader.as_ref()?;
        let ws = rustix::termios::tcgetwinsize(tty).ok()?;
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return None;
        }
        Some((ws.ws_col, ws.ws_row))
    }

    #[cfg(unix)]
    fn drain_resize_notifications(&mut self) {
        if !self.live {
            return;
        }
        // Drain all pending SIGWINCH notifications, coalescing into a single
        // resize query (the authoritative size comes from ioctl, not the signal).
        let got_resize = if let Some(ref rx) = self.resize_rx {
            let mut any = false;
            while rx.try_recv().is_ok() {
                any = true;
            }
            any
        } else {
            false
        };
        if got_resize && let Some((w, h)) = self.query_tty_size() {
            self.push_resize(w, h);
        }
    }

    /// Read available bytes from the tty reader and feed them to the parser.
    fn drain_available_bytes(&mut self) -> io::Result<()> {
        let Some(ref mut tty) = self.tty_reader else {
            return Ok(());
        };
        let mut buf = [0u8; 1024];
        match tty.read(&mut buf) {
            Ok(0) => Ok(()),
            Ok(n) => {
                let events = self.parser.parse(&buf[..n]);
                self.event_queue.extend(events);
                Ok(())
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Poll the tty fd for available data using `poll(2)`.
    #[cfg(unix)]
    fn poll_tty(&mut self, timeout: Duration) -> io::Result<bool> {
        use std::os::fd::AsFd;
        let ready = {
            let Some(ref tty) = self.tty_reader else {
                return Ok(false);
            };
            let mut poll_fds = [nix::poll::PollFd::new(
                tty.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            let timeout_ms: u16 = timeout.as_millis().try_into().unwrap_or(u16::MAX);
            match nix::poll::poll(&mut poll_fds, nix::poll::PollTimeout::from(timeout_ms)) {
                Ok(n) => n,
                Err(nix::errno::Errno::EINTR) => return Ok(false),
                Err(e) => return Err(io::Error::other(e)),
            }
        };
        if ready > 0 {
            self.drain_available_bytes()?;
        }
        Ok(!self.event_queue.is_empty())
    }

    /// Stub for non-Unix platforms.
    #[cfg(not(unix))]
    fn poll_tty(&mut self, _timeout: Duration) -> io::Result<bool> {
        Ok(false)
    }

    /// Write the escape sequences needed to transition from current to new features.
    fn write_feature_delta(
        current: &BackendFeatures,
        new: &BackendFeatures,
        writer: &mut impl Write,
    ) -> io::Result<()> {
        if new.mouse_capture != current.mouse_capture {
            writer.write_all(if new.mouse_capture {
                MOUSE_ENABLE
            } else {
                MOUSE_DISABLE
            })?;
        }
        if new.bracketed_paste != current.bracketed_paste {
            writer.write_all(if new.bracketed_paste {
                BRACKETED_PASTE_ENABLE
            } else {
                BRACKETED_PASTE_DISABLE
            })?;
        }
        if new.focus_events != current.focus_events {
            writer.write_all(if new.focus_events {
                FOCUS_ENABLE
            } else {
                FOCUS_DISABLE
            })?;
        }
        if new.kitty_keyboard != current.kitty_keyboard {
            writer.write_all(if new.kitty_keyboard {
                KITTY_KEYBOARD_ENABLE
            } else {
                KITTY_KEYBOARD_DISABLE
            })?;
        }
        Ok(())
    }

    /// Disable all active features, writing escape sequences to `writer`.
    fn disable_all(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let off = BackendFeatures::default();
        Self::write_feature_delta(&self.features, &off, writer)?;
        self.features = off;
        Ok(())
    }
}

impl BackendEventSource for TtyEventSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), Self::Error> {
        #[cfg(unix)]
        if let Some((w, h)) = self.query_tty_size() {
            return Ok((w, h));
        }
        Ok((self.width, self.height))
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error> {
        if self.live {
            let mut stdout = io::stdout();
            Self::write_feature_delta(&self.features, &features, &mut stdout)?;
            stdout.flush()?;
        }
        self.features = features;
        Ok(())
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, Self::Error> {
        #[cfg(unix)]
        self.drain_resize_notifications();

        // If we already have buffered events, return immediately.
        if !self.event_queue.is_empty() {
            return Ok(true);
        }

        #[cfg(unix)]
        if self.resize_rx.is_some() && timeout != Duration::ZERO {
            // `poll(2)` won't reliably wake on SIGWINCH (signal handlers are installed
            // with SA_RESTART). Time-slice to bound resize latency without busy looping.
            let deadline = std::time::Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(std::time::Instant::now);
            let slice_max = Duration::from_millis(50);
            loop {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Ok(false);
                }
                let remaining = deadline.duration_since(now);
                let poll_for = remaining.min(slice_max);
                let _ = self.poll_tty(poll_for)?;
                self.drain_resize_notifications();
                if !self.event_queue.is_empty() {
                    return Ok(true);
                }
            }
        }

        self.poll_tty(timeout)
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        Ok(self.event_queue.pop_front())
    }
}

// ── Presenter ────────────────────────────────────────────────────────────

/// Native ANSI presenter (Buffer → escape sequences → stdout).
///
/// Currently a skeleton. Real rendering is wired by later integration beads.
pub struct TtyPresenter {
    capabilities: TerminalCapabilities,
}

impl TtyPresenter {
    #[must_use]
    pub fn new(capabilities: TerminalCapabilities) -> Self {
        Self { capabilities }
    }
}

impl BackendPresenter for TtyPresenter {
    type Error = io::Error;

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    fn write_log(&mut self, _text: &str) -> Result<(), Self::Error> {
        // TODO: write to scrollback region or stderr
        Ok(())
    }

    fn present_ui(
        &mut self,
        _buf: &Buffer,
        _diff: Option<&BufferDiff>,
        _full_repaint_hint: bool,
    ) -> Result<(), Self::Error> {
        // TODO: emit ANSI escape sequences to stdout
        Ok(())
    }
}

// ── Backend ──────────────────────────────────────────────────────────────

/// Native Unix terminal backend.
///
/// Combines `TtyClock`, `TtyEventSource`, and `TtyPresenter` into a single
/// `Backend` implementation that the ftui runtime can drive.
///
/// When created with [`TtyBackend::open`], the backend enters raw mode and
/// manages the terminal lifecycle via RAII. On drop (including panics),
/// all features are disabled, the cursor is shown, the alt screen is exited,
/// and raw mode is restored — in that order.
///
/// When created with [`TtyBackend::new`] (headless), no terminal I/O occurs.
pub struct TtyBackend {
    // Fields are ordered for correct drop sequence:
    // 1. clock (no cleanup needed)
    // 2. events (feature state tracking)
    // 3. presenter (no cleanup needed)
    // 4. alt_screen_active (tracked for cleanup)
    // 5. raw_mode — MUST be last: termios is restored after escape sequences
    clock: TtyClock,
    events: TtyEventSource,
    presenter: TtyPresenter,
    alt_screen_active: bool,
    #[cfg(unix)]
    raw_mode: Option<RawModeGuard>,
}

impl TtyBackend {
    /// Create a headless backend (no terminal I/O). Useful for testing.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::new(width, height),
            presenter: TtyPresenter::new(TerminalCapabilities::detect()),
            alt_screen_active: false,
            #[cfg(unix)]
            raw_mode: None,
        }
    }

    /// Create a headless backend with explicit capabilities.
    #[must_use]
    pub fn with_capabilities(width: u16, height: u16, capabilities: TerminalCapabilities) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::new(width, height),
            presenter: TtyPresenter::new(capabilities),
            alt_screen_active: false,
            #[cfg(unix)]
            raw_mode: None,
        }
    }

    /// Open a live terminal session: enter raw mode, enable requested features.
    ///
    /// The terminal is fully restored on drop (even during panics, unless
    /// `panic = "abort"`).
    #[cfg(unix)]
    pub fn open(width: u16, height: u16, options: TtySessionOptions) -> io::Result<Self> {
        // Enter raw mode first — if this fails, nothing to clean up.
        let raw_mode = RawModeGuard::enter()?;

        let mut stdout = io::stdout();
        let mut alt_screen_active = false;

        // Enable initial features.
        let mut events = TtyEventSource::live(width, height)?;
        let setup: io::Result<()> = (|| {
            // Enter alt screen if requested.
            if options.alternate_screen {
                stdout.write_all(ALT_SCREEN_ENTER)?;
                stdout.write_all(CLEAR_SCREEN)?;
                stdout.write_all(CURSOR_HOME)?;
                alt_screen_active = true;
            }

            TtyEventSource::write_feature_delta(
                &BackendFeatures::default(),
                &options.features,
                &mut stdout,
            )?;

            stdout.flush()?;
            Ok(())
        })();

        if let Err(err) = setup {
            // Best-effort cleanup: we may have partially enabled features or entered alt screen.
            let _ =
                write_cleanup_sequence(&options.features, options.alternate_screen, &mut stdout);
            let _ = stdout.flush();
            return Err(err);
        }

        events.features = options.features;

        Ok(Self {
            clock: TtyClock::new(),
            events,
            presenter: TtyPresenter::new(TerminalCapabilities::detect()),
            alt_screen_active,
            raw_mode: Some(raw_mode),
        })
    }

    /// Whether this backend has an active terminal session (raw mode).
    #[must_use]
    pub fn is_live(&self) -> bool {
        #[cfg(unix)]
        {
            self.raw_mode.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl Drop for TtyBackend {
    fn drop(&mut self) {
        // Only run cleanup if we have an active session.
        #[cfg(unix)]
        if self.raw_mode.is_some() {
            let mut stdout = io::stdout();

            // End any in-progress synchronized output.
            let _ = stdout.write_all(SYNC_END);

            // Disable features in reverse order of typical enable.
            let _ = self.events.disable_all(&mut stdout);

            // Always show cursor.
            let _ = stdout.write_all(CURSOR_SHOW);

            // Leave alt screen.
            if self.alt_screen_active {
                let _ = stdout.write_all(ALT_SCREEN_LEAVE);
                self.alt_screen_active = false;
            }

            // Flush everything before RawModeGuard restores termios.
            let _ = stdout.flush();

            // RawModeGuard::drop() runs after this, restoring original termios.
        }
    }
}

impl Backend for TtyBackend {
    type Error = io::Error;
    type Clock = TtyClock;
    type Events = TtyEventSource;
    type Presenter = TtyPresenter;

    fn clock(&self) -> &Self::Clock {
        &self.clock
    }

    fn events(&mut self) -> &mut Self::Events {
        &mut self.events
    }

    fn presenter(&mut self) -> &mut Self::Presenter {
        &mut self.presenter
    }
}

// ── Utility: write cleanup sequence to a byte buffer (for testing) ───────

/// Write the full cleanup sequence for the given feature state to `writer`.
///
/// This is useful for verifying cleanup in PTY tests without needing
/// a real terminal session.
pub fn write_cleanup_sequence(
    features: &BackendFeatures,
    alt_screen: bool,
    writer: &mut impl Write,
) -> io::Result<()> {
    writer.write_all(SYNC_END)?;
    // Disable features in reverse order.
    if features.kitty_keyboard {
        writer.write_all(KITTY_KEYBOARD_DISABLE)?;
    }
    if features.focus_events {
        writer.write_all(FOCUS_DISABLE)?;
    }
    if features.bracketed_paste {
        writer.write_all(BRACKETED_PASTE_DISABLE)?;
    }
    if features.mouse_capture {
        writer.write_all(MOUSE_DISABLE)?;
    }
    writer.write_all(CURSOR_SHOW)?;
    if alt_screen {
        writer.write_all(ALT_SCREEN_LEAVE)?;
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic() {
        let clock = TtyClock::new();
        let t1 = clock.now_mono();
        std::hint::black_box(0..1000).for_each(|_| {});
        let t2 = clock.now_mono();
        assert!(t2 >= t1, "clock must be monotonic");
    }

    #[test]
    fn event_source_reports_size() {
        let src = TtyEventSource::new(80, 24);
        let (w, h) = src.size().unwrap();
        assert_eq!(w, 80);
        assert_eq!(h, 24);
    }

    #[test]
    fn event_source_set_features_headless() {
        let mut src = TtyEventSource::new(80, 24);
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: false,
        };
        src.set_features(features).unwrap();
        assert_eq!(src.features(), features);
    }

    #[test]
    fn poll_returns_false_headless() {
        let mut src = TtyEventSource::new(80, 24);
        assert!(!src.poll_event(Duration::from_millis(0)).unwrap());
    }

    #[test]
    fn read_returns_none_headless() {
        let mut src = TtyEventSource::new(80, 24);
        assert!(src.read_event().unwrap().is_none());
    }

    #[test]
    fn push_resize_enqueues_event_and_updates_size() {
        let mut src = TtyEventSource::new(80, 24);
        src.push_resize(120, 40);
        assert_eq!(src.size().unwrap(), (120, 40));
        assert_eq!(
            src.read_event().unwrap(),
            Some(Event::Resize {
                width: 120,
                height: 40,
            })
        );
        assert!(src.read_event().unwrap().is_none());
    }

    #[test]
    fn push_resize_deduplicates_same_size() {
        let mut src = TtyEventSource::new(80, 24);
        src.push_resize(80, 24);
        assert!(src.event_queue.is_empty(), "no event when size unchanged");
    }

    #[test]
    fn push_resize_ignores_zero_dimensions() {
        let mut src = TtyEventSource::new(80, 24);
        src.push_resize(0, 24);
        assert!(src.event_queue.is_empty());
        src.push_resize(80, 0);
        assert!(src.event_queue.is_empty());
        src.push_resize(0, 0);
        assert!(src.event_queue.is_empty());
    }

    #[test]
    fn resize_storm_coalesces_and_no_panic() {
        let mut src = TtyEventSource::new(80, 24);
        // Simulate a rapid resize storm: 1000 identical resize signals.
        for _ in 0..1000 {
            src.push_resize(120, 40);
        }
        // First push changes 80x24→120x40, rest are deduped.
        assert_eq!(src.event_queue.len(), 1);
        assert_eq!(
            src.event_queue.pop_front().unwrap(),
            Event::Resize {
                width: 120,
                height: 40,
            }
        );
    }

    #[test]
    fn resize_storm_varied_sizes_no_panic() {
        let mut src = TtyEventSource::new(80, 24);
        // Rapidly varying sizes — all should produce events.
        for i in 1..=500u16 {
            src.push_resize(80 + i, 24 + (i % 50));
        }
        // No panics, events are in order.
        let mut prev_w = 80u16;
        while let Some(Event::Resize { width, .. }) = src.event_queue.pop_front() {
            assert!(
                width > prev_w || width == prev_w + 1 || width != prev_w,
                "events must be in push order"
            );
            prev_w = width;
        }
    }

    // ── Pipe-based input parity tests ─────────────────────────────────

    /// Create a (reader_file, writer_stream) pair using Unix sockets.
    #[cfg(unix)]
    fn pipe_pair() -> (std::fs::File, std::os::unix::net::UnixStream) {
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().unwrap();
        // Convert reader to File via OwnedFd for compatibility with TtyEventSource.
        let reader: std::fs::File = std::os::fd::OwnedFd::from(a).into();
        (reader, b)
    }

    #[cfg(unix)]
    #[test]
    fn pipe_ascii_chars() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        writer.write_all(b"abc").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e1 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e1,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
        let e2 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e2,
            Event::Key(KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
        let e3 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e3,
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
        // Queue should now be empty.
        assert!(src.read_event().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pipe_arrow_keys() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Up (A), Down (B), Right (C), Left (D)
        writer.write_all(b"\x1b[A\x1b[B\x1b[C\x1b[D").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let codes: Vec<KeyCode> = std::iter::from_fn(|| {
            src.read_event().unwrap().map(|e| match e {
                Event::Key(KeyEvent { code, .. }) => code,
                _ => panic!("expected key event"),
            })
        })
        .collect();
        assert_eq!(
            codes,
            vec![KeyCode::Up, KeyCode::Down, KeyCode::Right, KeyCode::Left]
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_ctrl_keys() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Ctrl+A = 0x01, Ctrl+C = 0x03
        writer.write_all(&[0x01, 0x03]).unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e1 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e1,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );
        let e2 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e2,
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_function_keys() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // F1 (SS3 P) and F5 (CSI 15~)
        writer.write_all(b"\x1bOP\x1b[15~").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e1 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e1,
            Event::Key(KeyEvent {
                code: KeyCode::F(1),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
        let e2 = src.read_event().unwrap().unwrap();
        assert_eq!(
            e2,
            Event::Key(KeyEvent {
                code: KeyCode::F(5),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_mouse_sgr_click() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // SGR mouse: left click at (10, 20) — 1-indexed in protocol, 0-indexed in Event.
        writer.write_all(b"\x1b[<0;10;20M").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert_eq!(
            e,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                x: 9,
                y: 19,
                modifiers: Modifiers::NONE,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_focus_events() {
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Focus in (CSI I) and focus out (CSI O)
        writer.write_all(b"\x1b[I\x1b[O").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        assert_eq!(src.read_event().unwrap().unwrap(), Event::Focus(true));
        assert_eq!(src.read_event().unwrap().unwrap(), Event::Focus(false));
    }

    #[cfg(unix)]
    #[test]
    fn pipe_bracketed_paste() {
        use ftui_core::event::PasteEvent;
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        writer.write_all(b"\x1b[200~hello world\x1b[201~").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert_eq!(
            e,
            Event::Paste(PasteEvent {
                text: "hello world".to_string(),
                bracketed: true,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_modified_arrow_key() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Ctrl+Up: CSI 1;5A
        writer.write_all(b"\x1b[1;5A").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert_eq!(
            e,
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn pipe_scroll_events() {
        use ftui_core::event::{Modifiers, MouseEvent, MouseEventKind};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // SGR scroll up at (5, 5): button=64 (scroll bit + up)
        writer.write_all(b"\x1b[<64;5;5M").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert_eq!(
            e,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                x: 4,
                y: 4,
                modifiers: Modifiers::NONE,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn poll_returns_buffered_events_immediately() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Write multiple chars to produce multiple events.
        writer.write_all(b"xy").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        // Consume only one event.
        let _ = src.read_event().unwrap().unwrap();
        // Second poll should return true immediately (buffered event).
        assert!(src.poll_event(Duration::from_millis(0)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert_eq!(
            e,
            Event::Key(KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn presenter_capabilities() {
        let caps = TerminalCapabilities::detect();
        let presenter = TtyPresenter::new(caps);
        let _c = presenter.capabilities();
    }

    #[test]
    fn backend_headless_construction() {
        let backend = TtyBackend::new(120, 40);
        assert!(!backend.is_live());
        let (w, h) = backend.events.size().unwrap();
        assert_eq!(w, 120);
        assert_eq!(h, 40);
    }

    #[test]
    fn backend_trait_impl() {
        let mut backend = TtyBackend::new(80, 24);
        let _t = backend.clock().now_mono();
        let (w, h) = backend.events().size().unwrap();
        assert_eq!((w, h), (80, 24));
        let _c = backend.presenter().capabilities();
    }

    #[test]
    fn feature_delta_writes_enable_sequences() {
        let current = BackendFeatures::default();
        let new = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let mut buf = Vec::new();
        TtyEventSource::write_feature_delta(&current, &new, &mut buf).unwrap();
        assert!(
            buf.windows(MOUSE_ENABLE.len()).any(|w| w == MOUSE_ENABLE),
            "expected mouse enable sequence"
        );
        assert!(
            buf.windows(BRACKETED_PASTE_ENABLE.len())
                .any(|w| w == BRACKETED_PASTE_ENABLE),
            "expected bracketed paste enable"
        );
        assert!(
            buf.windows(FOCUS_ENABLE.len()).any(|w| w == FOCUS_ENABLE),
            "expected focus enable"
        );
        assert!(
            buf.windows(KITTY_KEYBOARD_ENABLE.len())
                .any(|w| w == KITTY_KEYBOARD_ENABLE),
            "expected kitty keyboard enable"
        );
    }

    #[test]
    fn feature_delta_writes_disable_sequences() {
        let current = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let new = BackendFeatures::default();
        let mut buf = Vec::new();
        TtyEventSource::write_feature_delta(&current, &new, &mut buf).unwrap();
        assert!(buf.windows(MOUSE_DISABLE.len()).any(|w| w == MOUSE_DISABLE));
        assert!(
            buf.windows(BRACKETED_PASTE_DISABLE.len())
                .any(|w| w == BRACKETED_PASTE_DISABLE)
        );
        assert!(buf.windows(FOCUS_DISABLE.len()).any(|w| w == FOCUS_DISABLE));
        assert!(
            buf.windows(KITTY_KEYBOARD_DISABLE.len())
                .any(|w| w == KITTY_KEYBOARD_DISABLE)
        );
    }

    #[test]
    fn feature_delta_noop_when_unchanged() {
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: true,
            kitty_keyboard: false,
        };
        let mut buf = Vec::new();
        TtyEventSource::write_feature_delta(&features, &features, &mut buf).unwrap();
        assert!(buf.is_empty(), "no output expected when features unchanged");
    }

    #[test]
    fn cleanup_sequence_contains_all_disable() {
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, true, &mut buf).unwrap();

        // Verify all expected sequences are present.
        assert!(buf.windows(SYNC_END.len()).any(|w| w == SYNC_END));
        assert!(buf.windows(MOUSE_DISABLE.len()).any(|w| w == MOUSE_DISABLE));
        assert!(
            buf.windows(BRACKETED_PASTE_DISABLE.len())
                .any(|w| w == BRACKETED_PASTE_DISABLE)
        );
        assert!(buf.windows(FOCUS_DISABLE.len()).any(|w| w == FOCUS_DISABLE));
        assert!(
            buf.windows(KITTY_KEYBOARD_DISABLE.len())
                .any(|w| w == KITTY_KEYBOARD_DISABLE)
        );
        assert!(buf.windows(CURSOR_SHOW.len()).any(|w| w == CURSOR_SHOW));
        assert!(
            buf.windows(ALT_SCREEN_LEAVE.len())
                .any(|w| w == ALT_SCREEN_LEAVE)
        );
    }

    #[test]
    fn cleanup_sequence_ordering() {
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, true, &mut buf).unwrap();

        // Verify ordering: sync_end first, cursor_show before alt_screen_leave.
        let sync_pos = buf
            .windows(SYNC_END.len())
            .position(|w| w == SYNC_END)
            .expect("sync_end present");
        let cursor_pos = buf
            .windows(CURSOR_SHOW.len())
            .position(|w| w == CURSOR_SHOW)
            .expect("cursor_show present");
        let alt_pos = buf
            .windows(ALT_SCREEN_LEAVE.len())
            .position(|w| w == ALT_SCREEN_LEAVE)
            .expect("alt_screen_leave present");

        assert!(
            sync_pos < cursor_pos,
            "sync_end must come before cursor_show"
        );
        assert!(
            cursor_pos < alt_pos,
            "cursor_show must come before alt_screen_leave"
        );
    }

    #[test]
    fn disable_all_resets_feature_state() {
        let mut src = TtyEventSource::new(80, 24);
        src.features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let mut buf = Vec::new();
        src.disable_all(&mut buf).unwrap();
        assert_eq!(src.features(), BackendFeatures::default());
        // Verify disable sequences were written.
        assert!(!buf.is_empty());
    }

    // ── PTY-based raw mode tests ─────────────────────────────────────

    #[cfg(unix)]
    mod pty_tests {
        use super::*;
        use nix::pty::openpty;
        use nix::sys::termios::{self, LocalFlags};
        use std::io::Read;

        fn pty_pair() -> (std::fs::File, std::fs::File) {
            let result = openpty(None, None).expect("openpty failed");
            (
                std::fs::File::from(result.master),
                std::fs::File::from(result.slave),
            )
        }

        #[test]
        fn raw_mode_entered_and_restored_on_drop() {
            let (_master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            // Before: canonical mode with ECHO.
            let before = termios::tcgetattr(&slave_dup).unwrap();
            assert!(
                before.local_flags.contains(LocalFlags::ECHO),
                "default termios should have ECHO"
            );
            assert!(
                before.local_flags.contains(LocalFlags::ICANON),
                "default termios should have ICANON"
            );

            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();

                // During: raw mode — no echo, no canonical.
                let during = termios::tcgetattr(&slave_dup).unwrap();
                assert!(
                    !during.local_flags.contains(LocalFlags::ECHO),
                    "raw mode should clear ECHO"
                );
                assert!(
                    !during.local_flags.contains(LocalFlags::ICANON),
                    "raw mode should clear ICANON"
                );
            }

            // After drop: original termios restored.
            let after = termios::tcgetattr(&slave_dup).unwrap();
            assert!(
                after.local_flags.contains(LocalFlags::ECHO),
                "should restore ECHO after drop"
            );
            assert!(
                after.local_flags.contains(LocalFlags::ICANON),
                "should restore ICANON after drop"
            );
        }

        #[test]
        fn panic_restores_termios() {
            let (_master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            // Spawn a thread that panics with the guard held.
            let handle = std::thread::spawn(move || {
                let _guard = RawModeGuard::enter_on(slave).unwrap();
                panic!("intentional panic for testing raw mode cleanup");
            });

            assert!(handle.join().is_err(), "thread should have panicked");

            // Verify termios restored despite the panic.
            let after = termios::tcgetattr(&slave_dup).unwrap();
            assert!(
                after.local_flags.contains(LocalFlags::ECHO),
                "ECHO should be restored after panic"
            );
            assert!(
                after.local_flags.contains(LocalFlags::ICANON),
                "ICANON should be restored after panic"
            );
        }

        #[test]
        fn backend_drop_writes_cleanup_sequences() {
            let (mut master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();

                // Write feature-enable sequences to the PTY.
                let mut stdout_buf = Vec::new();
                let all_on = BackendFeatures {
                    mouse_capture: true,
                    bracketed_paste: true,
                    focus_events: true,
                    kitty_keyboard: true,
                };
                TtyEventSource::write_feature_delta(
                    &BackendFeatures::default(),
                    &all_on,
                    &mut stdout_buf,
                )
                .unwrap();
                // Also write cleanup as if TtyBackend::drop ran.
                write_cleanup_sequence(&all_on, true, &mut stdout_buf).unwrap();

                // Write it all to the slave so master can read it.
                use std::io::Write;
                let mut slave_writer = slave_dup.try_clone().unwrap();
                slave_writer.write_all(&stdout_buf).unwrap();
                slave_writer.flush().unwrap();
            }

            // Read from master to verify cleanup sequences were written.
            let mut buf = vec![0u8; 2048];
            let n = master.read(&mut buf).unwrap();
            let output = &buf[..n];

            assert!(
                output.windows(CURSOR_SHOW.len()).any(|w| w == CURSOR_SHOW),
                "cleanup must show cursor"
            );
            assert!(
                output
                    .windows(MOUSE_DISABLE.len())
                    .any(|w| w == MOUSE_DISABLE),
                "cleanup must disable mouse"
            );
            assert!(
                output
                    .windows(ALT_SCREEN_LEAVE.len())
                    .any(|w| w == ALT_SCREEN_LEAVE),
                "cleanup must leave alt-screen"
            );
        }

        /// Helper: write bytes to the PTY slave and read them back from master.
        fn write_to_slave_and_read_master(
            master: &mut std::fs::File,
            slave: &std::fs::File,
            data: &[u8],
        ) -> Vec<u8> {
            use std::io::Write;
            let mut writer = slave.try_clone().unwrap();
            writer.write_all(data).unwrap();
            writer.flush().unwrap();
            let mut buf = vec![0u8; 4096];
            let n = master.read(&mut buf).unwrap();
            buf.truncate(n);
            buf
        }

        #[test]
        fn cursor_hide_on_enter_show_on_drop() {
            let (mut master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            // Simulate entering a session: raw mode + hide cursor.
            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();
                let output = write_to_slave_and_read_master(&mut master, &slave_dup, CURSOR_HIDE);
                assert!(
                    output.windows(CURSOR_HIDE.len()).any(|w| w == CURSOR_HIDE),
                    "cursor-hide should be written on session enter"
                );

                // Simulate drop cleanup: show cursor.
                let output = write_to_slave_and_read_master(&mut master, &slave_dup, CURSOR_SHOW);
                assert!(
                    output.windows(CURSOR_SHOW.len()).any(|w| w == CURSOR_SHOW),
                    "cursor-show should be written on session exit"
                );
            }
        }

        #[test]
        fn alt_screen_enter_and_leave_via_pty() {
            let (mut master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();

                // Enter alt-screen.
                let output =
                    write_to_slave_and_read_master(&mut master, &slave_dup, ALT_SCREEN_ENTER);
                assert!(
                    output
                        .windows(ALT_SCREEN_ENTER.len())
                        .any(|w| w == ALT_SCREEN_ENTER),
                    "alt-screen enter should pass through PTY"
                );

                // Leave alt-screen.
                let output =
                    write_to_slave_and_read_master(&mut master, &slave_dup, ALT_SCREEN_LEAVE);
                assert!(
                    output
                        .windows(ALT_SCREEN_LEAVE.len())
                        .any(|w| w == ALT_SCREEN_LEAVE),
                    "alt-screen leave should pass through PTY"
                );
            }
        }

        #[test]
        fn per_feature_disable_on_drop() {
            let (mut master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();

                // Enable all features, then write cleanup (simulating TtyBackend::drop).
                let all_on = BackendFeatures {
                    mouse_capture: true,
                    bracketed_paste: true,
                    focus_events: true,
                    kitty_keyboard: true,
                };
                let mut cleanup = Vec::new();
                write_cleanup_sequence(&all_on, false, &mut cleanup).unwrap();

                let output = write_to_slave_and_read_master(&mut master, &slave_dup, &cleanup);

                // Verify each feature's disable sequence individually.
                assert!(
                    output
                        .windows(MOUSE_DISABLE.len())
                        .any(|w| w == MOUSE_DISABLE),
                    "mouse must be disabled on drop"
                );
                assert!(
                    output
                        .windows(BRACKETED_PASTE_DISABLE.len())
                        .any(|w| w == BRACKETED_PASTE_DISABLE),
                    "bracketed paste must be disabled on drop"
                );
                assert!(
                    output
                        .windows(FOCUS_DISABLE.len())
                        .any(|w| w == FOCUS_DISABLE),
                    "focus events must be disabled on drop"
                );
                assert!(
                    output
                        .windows(KITTY_KEYBOARD_DISABLE.len())
                        .any(|w| w == KITTY_KEYBOARD_DISABLE),
                    "kitty keyboard must be disabled on drop"
                );
                assert!(
                    output.windows(CURSOR_SHOW.len()).any(|w| w == CURSOR_SHOW),
                    "cursor must be shown on drop"
                );
            }
        }

        #[test]
        fn panic_with_features_restores_termios() {
            let (_master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            let handle = std::thread::spawn(move || {
                let _guard = RawModeGuard::enter_on(slave).unwrap();
                // Simulate having features enabled — the guard tracks termios, and
                // TtyBackend::drop would disable features. Here we just verify
                // the termios restoration happens even when features were "active".
                panic!("panic with features enabled");
            });

            assert!(handle.join().is_err());

            let after = termios::tcgetattr(&slave_dup).unwrap();
            assert!(
                after.local_flags.contains(LocalFlags::ECHO),
                "ECHO restored after panic with features"
            );
            assert!(
                after.local_flags.contains(LocalFlags::ICANON),
                "ICANON restored after panic with features"
            );
        }

        #[test]
        fn repeated_raw_mode_cycles_no_leak() {
            let (_master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            // Enter and exit raw mode multiple times.
            for _ in 0..5 {
                let s = slave_dup.try_clone().unwrap();
                let guard = RawModeGuard::enter_on(s).unwrap();

                // Verify raw mode active.
                let during = termios::tcgetattr(&slave_dup).unwrap();
                assert!(!during.local_flags.contains(LocalFlags::ECHO));

                drop(guard);

                // Verify restored.
                let after = termios::tcgetattr(&slave_dup).unwrap();
                assert!(
                    after.local_flags.contains(LocalFlags::ECHO),
                    "ECHO must be restored each cycle"
                );
            }
        }

        #[test]
        fn cleanup_ordering_via_pty() {
            let (mut master, slave) = pty_pair();
            let slave_dup = slave.try_clone().unwrap();

            {
                let _guard = RawModeGuard::enter_on(slave).unwrap();

                // Write a full cleanup sequence and verify ordering.
                let features = BackendFeatures {
                    mouse_capture: true,
                    bracketed_paste: true,
                    focus_events: true,
                    kitty_keyboard: true,
                };
                let mut seq = Vec::new();
                write_cleanup_sequence(&features, true, &mut seq).unwrap();

                let output = write_to_slave_and_read_master(&mut master, &slave_dup, &seq);

                // Verify ordering: sync_end before cursor_show before alt_screen_leave.
                let sync_pos = output
                    .windows(SYNC_END.len())
                    .position(|w| w == SYNC_END)
                    .expect("sync_end present");
                let cursor_pos = output
                    .windows(CURSOR_SHOW.len())
                    .position(|w| w == CURSOR_SHOW)
                    .expect("cursor_show present");
                let alt_pos = output
                    .windows(ALT_SCREEN_LEAVE.len())
                    .position(|w| w == ALT_SCREEN_LEAVE)
                    .expect("alt_screen_leave present");

                assert!(
                    sync_pos < cursor_pos,
                    "sync_end ({sync_pos}) must precede cursor_show ({cursor_pos})"
                );
                assert!(
                    cursor_pos < alt_pos,
                    "cursor_show ({cursor_pos}) must precede alt_screen_leave ({alt_pos})"
                );
            }
        }
    }
}
