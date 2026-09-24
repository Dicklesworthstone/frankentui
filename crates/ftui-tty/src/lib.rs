#![forbid(unsafe_code)]
// Unix-first; Windows is deferred (see docs/WINDOWS.md). The whole backend is
// compiled out on other targets so the crate stays a no-op dependency there
// instead of a pile of dead, unix-only helpers that fail clippy on Windows.
#![cfg(unix)]
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
//! | Mouse (SGR)       | `CSI ? 1000;1002;1006 h` (+ split compatibility) | `CSI ? 1000;1002;1006 l` (+ legacy reset hygiene) |
//! | Bracketed paste   | `CSI ? 2004 h`            | `CSI ? 2004 l`            |
//! | Focus events      | `CSI ? 1004 h`            | `CSI ? 1004 l`            |
//! | Kitty keyboard    | `CSI > 15 u`              | `CSI < u`                 |
//! | Cursor show/hide  | `CSI ? 25 h`              | `CSI ? 25 l`              |
//! | Sync output       | `CSI ? 2026 h`            | `CSI ? 2026 l`            |

use core::time::Duration;
use std::collections::VecDeque;
use std::io::{self, BufWriter, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use ftui_backend::{Backend, BackendClock, BackendEventSource, BackendFeatures};
use ftui_core::event::{Event, MouseEventKind};
use ftui_core::input_parser::InputParser;
use ftui_core::session_teardown::seq::{
    ALT_SCREEN_ENTER, BRACKETED_PASTE_DISABLE, BRACKETED_PASTE_ENABLE, FOCUS_DISABLE, FOCUS_ENABLE,
    KITTY_KEYBOARD_DISABLE, KITTY_KEYBOARD_ENABLE, MOUSE_DISABLE, MOUSE_DISABLE_MUX_SAFE,
    MOUSE_ENABLE, MOUSE_ENABLE_MUX_SAFE,
};
use ftui_core::session_teardown::{KittyPopLatch, TeardownPlan, install_chained_panic_hook};
use ftui_core::terminal_capabilities::TerminalCapabilities;
#[cfg(test)]
use ftui_render::buffer::Buffer;

#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
#[cfg(unix)]
use signal_hook::iterator::Signals;

#[cfg(unix)]
static TTY_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

// ── Debug Input Tracing ──────────────────────────────────────────────────

const INPUT_TRACE_ENV: &str = "FTUI_TTY_INPUT_TRACE";
const SIGNAL_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const SIGNAL_SHUTDOWN_POLL: Duration = Duration::from_millis(10);
static LIVE_SIGNAL_INTERCEPT_SESSIONS: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
#[derive(Debug)]
struct SignalInterceptGuard {
    active: bool,
}

#[cfg(unix)]
impl SignalInterceptGuard {
    fn new(enabled: bool) -> Self {
        if enabled {
            LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_add(1, Ordering::SeqCst);
            install_termination_signal_hook();
        }
        Self { active: enabled }
    }

    fn disarm(&mut self) -> bool {
        let was_active = self.active;
        self.active = false;
        was_active
    }
}

#[cfg(unix)]
impl Drop for SignalInterceptGuard {
    fn drop(&mut self) {
        if self.active {
            LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
struct InputTrace {
    seq: u64,
    writer: BufWriter<std::fs::File>,
}

impl InputTrace {
    fn from_env() -> Option<Self> {
        let path = std::env::var(INPUT_TRACE_ENV).ok()?;
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return None;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(trimmed)
            .ok()?;
        Some(Self {
            seq: 0,
            writer: BufWriter::new(file),
        })
    }

    fn record(&mut self, bytes: &[u8], parsed: &[Event]) {
        self.seq = self.seq.saturating_add(1);
        let _ = write!(self.writer, "seq={} n={} hex=", self.seq, bytes.len());
        let _ = write_hex(&mut self.writer, bytes);
        let _ = writeln!(self.writer);
        for ev in parsed {
            let _ = writeln!(self.writer, "  {:?}", ev);
        }
        let _ = writeln!(self.writer, "---");
        let _ = self.writer.flush();
    }
}

fn write_hex(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        w.write_all(&[HEX[(b >> 4) as usize], HEX[(b & 0x0f) as usize]])?;
    }
    Ok(())
}

#[inline]
const fn mouse_disable_sequence_for_capabilities(
    capabilities: TerminalCapabilities,
) -> &'static [u8] {
    if capabilities.in_any_mux() {
        MOUSE_DISABLE_MUX_SAFE
    } else {
        MOUSE_DISABLE
    }
}

#[inline]
const fn mouse_enable_sequence_for_capabilities(
    capabilities: TerminalCapabilities,
) -> &'static [u8] {
    if capabilities.in_any_mux() {
        MOUSE_ENABLE_MUX_SAFE
    } else {
        MOUSE_ENABLE
    }
}

#[inline]
const fn sanitize_feature_request(
    requested: BackendFeatures,
    capabilities: TerminalCapabilities,
) -> BackendFeatures {
    // Conservative policy for terminal mode toggles:
    // - Never request unsupported modes.
    // - Keep kitty keyboard off in all mux environments.
    // - Keep focus events off in all mux contexts; passthrough behavior varies.
    let focus_events_supported = capabilities.focus_events && !capabilities.in_any_mux();
    let kitty_keyboard_supported = capabilities.kitty_keyboard && !capabilities.in_any_mux();

    BackendFeatures {
        mouse_capture: requested.mouse_capture && capabilities.mouse_sgr,
        bracketed_paste: requested.bracketed_paste && capabilities.bracketed_paste,
        focus_events: requested.focus_events && focus_events_supported,
        kitty_keyboard: requested.kitty_keyboard && kitty_keyboard_supported,
    }
}

#[inline]
const fn conservative_feature_union(a: BackendFeatures, b: BackendFeatures) -> BackendFeatures {
    BackendFeatures {
        mouse_capture: a.mouse_capture || b.mouse_capture,
        bracketed_paste: a.bracketed_paste || b.bracketed_paste,
        focus_events: a.focus_events || b.focus_events,
        kitty_keyboard: a.kitty_keyboard || b.kitty_keyboard,
    }
}

const CLEAR_SCREEN: &[u8] = b"\x1b[2J";
const CURSOR_HOME: &[u8] = b"\x1b[H";
const READ_BUFFER_BYTES: usize = 8192;
const MAX_DRAIN_BYTES_PER_POLL: usize = READ_BUFFER_BYTES;
const INFERRED_PIXEL_WIDTH_PER_CELL: u16 = 8;
const INFERRED_PIXEL_HEIGHT_PER_CELL: u16 = 16;
/// Grace period before resolving a pending ambiguous escape/UTF-8 sequence.
///
/// This prevents split escape sequences (`ESC` then `[` in a later read) from
/// being prematurely emitted as a literal Escape key event.
const PARSER_TIMEOUT_GRACE: Duration = Duration::from_millis(50);

#[cfg(unix)]
fn raw_mode_snapshot_slot() -> &'static Mutex<Option<nix::sys::termios::Termios>> {
    static SLOT: OnceLock<Mutex<Option<nix::sys::termios::Termios>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

#[cfg(unix)]
fn store_raw_mode_snapshot(termios: &nix::sys::termios::Termios) {
    let slot = raw_mode_snapshot_slot();
    let mut guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    *guard = Some(termios.clone());
}

#[cfg(unix)]
fn clear_raw_mode_snapshot() {
    let slot = raw_mode_snapshot_slot();
    let mut guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    *guard = None;
}

#[cfg(unix)]
fn restore_raw_mode_snapshot() {
    let slot = raw_mode_snapshot_slot();
    let snapshot = {
        let guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
        guard.clone()
    };

    let Some(original) = snapshot else {
        return;
    };

    let Ok(tty) = std::fs::File::open("/dev/tty") else {
        return;
    };
    // TCSANOW, not TCSAFLUSH: this runs from a signal handler, and TCSAFLUSH
    // waits for the output queue to drain before it applies. A terminal that
    // has stopped reading - flow control, a suspended emulator, a pipe nobody
    // is draining - would hang the handler here and leave the tty raw, which
    // is the exact failure this function exists to prevent. Restoring does not
    // need to drain: queued output is still transmitted, we simply do not
    // block on it, and discarding unread input would eat the user's typeahead.
    let _ = nix::sys::termios::tcsetattr(&tty, nix::sys::termios::SetArg::TCSANOW, &original);
}

#[cfg(unix)]
fn best_effort_termination_cleanup() {
    if !TTY_SESSION_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut stdout = io::stdout();
    let caps = TerminalCapabilities::with_overrides();
    // This path cannot prove ownership of an active sync block; avoid emitting
    // standalone DEC ?2026l during panic/signal cleanup. Extracting the shared
    // TeardownPlan in bd-g00-root-epic-ewths.17.6 flipped this to true, which
    // fails vfx_shape3d_wezterm_mux_policy_omits_sync_output_sequences: under a
    // multiplexer identity the policy forbids the sequence outright, and there
    // is no open block here for it to close even when it does not.
    let mut plan = TeardownPlan::from_capabilities(&caps, true, false);
    if !KittyPopLatch::try_claim() {
        plan.pop_kitty_keyboard = false;
    }
    let _ = plan.write_for_backend(&mut stdout, "tty");
    let _ = stdout.flush();
    restore_raw_mode_snapshot();
}

#[cfg(unix)]
fn install_abort_panic_hook() {
    install_chained_panic_hook("ftui-tty", best_effort_termination_cleanup);
}

#[cfg(unix)]
fn install_termination_signal_hook() {
    static HOOK: OnceLock<()> = OnceLock::new();
    HOOK.get_or_init(|| {
        let mut signals = match Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT]) {
            Ok(signals) => signals,
            Err(_) => return,
        };
        let _ = std::thread::Builder::new()
            .name("ftui-tty-term-signal".to_string())
            .spawn(move || {
                for signal in signals.forever() {
                    if LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst) == 0 {
                        std::process::exit(128 + signal);
                    }

                    ftui_core::shutdown_signal::record_pending_termination_signal(signal);
                    best_effort_termination_cleanup();
                    let deadline = std::time::Instant::now()
                        .checked_add(SIGNAL_SHUTDOWN_GRACE)
                        .unwrap_or_else(std::time::Instant::now);
                    loop {
                        if ftui_core::shutdown_signal::pending_termination_signal().is_none() {
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            std::process::exit(128 + signal);
                        }
                        std::thread::sleep(SIGNAL_SHUTDOWN_POLL);
                    }
                }
            });
    });
}

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

        store_raw_mode_snapshot(&original_termios);

        Ok(Self {
            original_termios,
            tty,
        })
    }

    /// Put back the terminal modes saved by [`enter`](Self::enter) while
    /// keeping the guard, so [`reenter_raw`](Self::reenter_raw) can undo it.
    /// Used to hand the terminal to the shell for a job-control stop.
    pub fn restore_original(&self) -> io::Result<()> {
        // TCSANOW for the reason given in `drop`: nothing to wait for.
        nix::sys::termios::tcsetattr(
            &self.tty,
            nix::sys::termios::SetArg::TCSANOW,
            &self.original_termios,
        )
        .map_err(io::Error::other)
    }

    /// Re-enter raw mode after [`restore_original`](Self::restore_original).
    pub fn reenter_raw(&self) -> io::Result<()> {
        let mut raw = self.original_termios.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        // TCSANOW, not the TCSAFLUSH of `enter_on`: input typed since the
        // process was continued is the user's, not stale typeahead.
        nix::sys::termios::tcsetattr(&self.tty, nix::sys::termios::SetArg::TCSANOW, &raw)
            .map_err(io::Error::other)
    }
}

#[cfg(unix)]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best-effort restore — ignore errors during cleanup.
        //
        // TCSANOW, not TCSAFLUSH: TCSAFLUSH blocks until the output queue has
        // drained, so dropping the guard while anything is still unread on the
        // other side of the tty deadlocks — the drain needs a reader, and the
        // reader is usually downstream of this drop returning. Entering raw
        // mode above keeps TCSAFLUSH, where discarding stale typeahead is the
        // point; on the way out there is nothing to wait for.
        let _ = nix::sys::termios::tcsetattr(
            &self.tty,
            nix::sys::termios::SetArg::TCSANOW,
            &self.original_termios,
        );
        clear_raw_mode_snapshot();
    }
}

// ── Session Options ──────────────────────────────────────────────────────

/// Configuration for opening a terminal session.
#[derive(Debug, Clone)]
pub struct TtySessionOptions {
    /// Enter the alternate screen buffer on open.
    pub alternate_screen: bool,
    /// Initial feature toggles to enable.
    pub features: BackendFeatures,
    /// Install a signal handler to restore terminal state on SIGINT/SIGTERM/SIGHUP.
    pub intercept_signals: bool,
}

impl Default for TtySessionOptions {
    fn default() -> Self {
        Self {
            alternate_screen: false,
            features: BackendFeatures::default(),
            intercept_signals: true,
        }
    }
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
    fn new(mut wake_writer: UnixStream) -> io::Result<Self> {
        wake_writer.set_nonblocking(true)?;
        let mut signals = Signals::new([SIGWINCH]).map_err(io::Error::other)?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            let pulse = [1u8; 1];
            for _ in signals.forever() {
                match wake_writer.write(&pulse) {
                    // The read side coalesces by draining all pending bytes before
                    // querying winsize, so any successful wake byte is enough.
                    Ok(_) => {}
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
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
    capabilities: TerminalCapabilities,
    width: u16,
    height: u16,
    /// Terminal pixel width reported by `TIOCGWINSZ` (0 if unavailable).
    pixel_width: u16,
    /// Terminal pixel height reported by `TIOCGWINSZ` (0 if unavailable).
    pixel_height: u16,
    /// Sticky detector for SGR-pixel mouse leakage (1016-style coordinates).
    ///
    /// Once we observe pixel-like coordinates, normalize subsequent mouse
    /// reports in this capture session so low-column clicks also map correctly.
    mouse_coords_pixels: bool,
    /// Inferred pixel width when `TIOCGWINSZ.ws_xpixel` is unavailable.
    ///
    /// Some terminals emit pixel-space SGR coordinates but report zero
    /// pixel geometry via winsize. Track observed maxima so we can project
    /// coordinates back into cells instead of clamping everything to edges.
    inferred_pixel_width: u16,
    /// Inferred pixel height when `TIOCGWINSZ.ws_ypixel` is unavailable.
    inferred_pixel_height: u16,
    /// When true, escape sequences are actually written to stdout.
    /// False in test/headless mode.
    live: bool,
    /// Read end of the resize wake stream.
    ///
    /// The SIGWINCH listener thread writes a byte here so the event loop can
    /// block in `poll(2)` until a resize actually happens.
    #[cfg(unix)]
    resize_reader: Option<UnixStream>,
    /// Owns the SIGWINCH handler thread (kept alive by this field).
    #[cfg(unix)]
    _resize_guard: Option<ResizeSignalGuard>,
    /// Parser state machine: decodes terminal byte sequences into Events.
    parser: InputParser,
    /// Buffered events from the most recent parse.
    event_queue: VecDeque<Event>,
    /// Tty file handle for reading input (None in headless mode).
    tty_reader: Option<std::fs::File>,
    /// True when tty_reader is configured as nonblocking and may be drained in a loop.
    reader_nonblocking: bool,
    /// Monotonic timestamp of the most recent byte read from the tty.
    last_input_byte_at: Option<Instant>,
    /// Optional raw input trace sink (env-gated).
    input_trace: Option<InputTrace>,
}

impl TtyEventSource {
    /// Create an event source in headless mode (no escape sequence output, no I/O).
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            features: BackendFeatures::default(),
            capabilities: TerminalCapabilities::basic(),
            width,
            height,
            pixel_width: 0,
            pixel_height: 0,
            mouse_coords_pixels: false,
            inferred_pixel_width: 0,
            inferred_pixel_height: 0,
            live: false,
            #[cfg(unix)]
            resize_reader: None,
            #[cfg(unix)]
            _resize_guard: None,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: None,
            reader_nonblocking: false,
            last_input_byte_at: None,
            input_trace: None,
        }
    }

    /// Create an event source in headless mode with explicit capabilities.
    #[must_use]
    pub fn with_capabilities(width: u16, height: u16, capabilities: TerminalCapabilities) -> Self {
        let mut source = Self::new(width, height);
        source.capabilities = capabilities;
        source
    }

    /// Create an event source in live mode (reads from /dev/tty, writes
    /// escape sequences to stdout).
    fn live(width: u16, height: u16, capabilities: TerminalCapabilities) -> io::Result<Self> {
        let tty_reader = std::fs::File::open("/dev/tty")?;
        let reader_nonblocking = Self::try_enable_nonblocking(&tty_reader);
        let mut w = width;
        let mut h = height;
        let mut pw = 0;
        let mut ph = 0;
        #[cfg(unix)]
        if let Ok(ws) = rustix::termios::tcgetwinsize(&tty_reader) {
            if ws.ws_col > 0 && ws.ws_row > 0 {
                w = ws.ws_col;
                h = ws.ws_row;
            }
            pw = ws.ws_xpixel;
            ph = ws.ws_ypixel;
        }

        #[cfg(unix)]
        let (resize_guard, resize_reader) = match UnixStream::pair() {
            Ok((resize_reader, resize_writer)) => {
                if resize_reader.set_nonblocking(true).is_ok() {
                    match ResizeSignalGuard::new(resize_writer) {
                        Ok(guard) => (Some(guard), Some(resize_reader)),
                        Err(_) => (None, None),
                    }
                } else {
                    (None, None)
                }
            }
            Err(_) => (None, None),
        };

        Ok(Self {
            features: BackendFeatures::default(),
            capabilities,
            width: w,
            height: h,
            pixel_width: pw,
            pixel_height: ph,
            mouse_coords_pixels: false,
            inferred_pixel_width: 0,
            inferred_pixel_height: 0,
            live: true,
            #[cfg(unix)]
            resize_reader,
            #[cfg(unix)]
            _resize_guard: resize_guard,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: Some(tty_reader),
            reader_nonblocking,
            last_input_byte_at: None,
            input_trace: InputTrace::from_env(),
        })
    }

    /// Create an event source that reads from an arbitrary file descriptor.
    ///
    /// Escape sequences are NOT written to stdout (headless feature toggle
    /// behavior). This is primarily useful for testing with pipes.
    #[cfg(test)]
    fn from_reader(width: u16, height: u16, reader: std::fs::File) -> Self {
        let reader_nonblocking = Self::try_enable_nonblocking(&reader);
        Self {
            features: BackendFeatures::default(),
            capabilities: TerminalCapabilities::basic(),
            width,
            height,
            pixel_width: 0,
            pixel_height: 0,
            mouse_coords_pixels: false,
            inferred_pixel_width: 0,
            inferred_pixel_height: 0,
            live: false,
            #[cfg(unix)]
            resize_reader: None,
            #[cfg(unix)]
            _resize_guard: None,
            parser: InputParser::new(),
            event_queue: VecDeque::new(),
            tty_reader: Some(reader),
            reader_nonblocking,
            last_input_byte_at: None,
            input_trace: None,
        }
    }

    #[cfg(unix)]
    fn try_enable_nonblocking(reader: &std::fs::File) -> bool {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

        let Ok(flags) = fcntl_getfl(reader) else {
            return false;
        };
        if flags.contains(OFlags::NONBLOCK) {
            return true;
        }
        fcntl_setfl(reader, flags | OFlags::NONBLOCK).is_ok()
    }

    #[cfg(not(unix))]
    fn try_enable_nonblocking(_reader: &std::fs::File) -> bool {
        false
    }

    /// Current feature state.
    #[must_use]
    pub fn features(&self) -> BackendFeatures {
        self.features
    }

    #[inline]
    fn sanitize_features(&self, requested: BackendFeatures) -> BackendFeatures {
        if !self.live {
            return requested;
        }
        sanitize_feature_request(requested, self.capabilities)
    }

    /// Apply feature state to internal parser/runtime flags without emitting
    /// terminal escape sequences.
    ///
    /// Keep both legacy mouse fallbacks enabled whenever mouse capture is on.
    ///
    /// Some terminals/mux stacks (notably Ghostty edge cases) can fall back to
    /// raw X10 `CSI M cb cx cy` packets despite SGR mode negotiation. We keep
    /// numeric legacy and X10 parsing active while capture is enabled so these
    /// sessions remain interactive.
    fn apply_feature_state(&mut self, features: BackendFeatures) {
        self.features = features;
        if !features.mouse_capture {
            self.mouse_coords_pixels = false;
            self.inferred_pixel_width = 0;
            self.inferred_pixel_height = 0;
        }
        self.parser.set_expect_x10_mouse(features.mouse_capture);
        // Always allow numeric legacy mouse fallback when capture is enabled.
        // Some terminals/muxes may ignore SGR mode requests in edge cases.
        self.parser.set_allow_legacy_mouse(features.mouse_capture);
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
        // Reset sticky pixel detection on resize.
        // If we previously entered pixel mode due to a resize race (clicks appearing "outside"
        // the old small bounds), we must give the terminal a chance to prove it's using
        // cells again against the new bounds.
        self.mouse_coords_pixels = false;
        self.inferred_pixel_width = 0;
        self.inferred_pixel_height = 0;
        self.event_queue.push_back(Event::Resize {
            width: new_width,
            height: new_height,
        });
    }

    /// Normalize mouse coordinates when terminals report SGR-pixel coordinates
    /// despite requesting cell mode.
    ///
    /// Some emulators can leak pixel-space coordinates (`1016h`) in mixed
    /// environments. If we detect obviously pixel-scale values and have tty
    /// pixel dimensions, project them back into the cell grid.
    fn normalize_event(&mut self, event: Event) -> Event {
        let Event::Mouse(mut mouse) = event else {
            return event;
        };

        let outside_grid = mouse.x >= self.width || mouse.y >= self.height;
        // Heuristic: Pixel coordinates are typically much larger than cell coordinates.
        // We use `width * 2` to scale with window size, but also enforce a static minimum (600)
        // to prevent false positives on wide terminals during resize races (e.g. clicking at
        // col 170 when internal width is still 80).
        let strongly_outside = (mouse.x >= self.width.saturating_mul(2)
            || mouse.y >= self.height.saturating_mul(2))
            && (mouse.x > 600 || mouse.y > 400);

        if !self.mouse_coords_pixels && strongly_outside {
            self.mouse_coords_pixels = true;
        }
        let likely_pixel_space = self.mouse_coords_pixels || strongly_outside;
        if !self.features.mouse_capture || !self.capabilities.mouse_sgr {
            return Event::Mouse(mouse);
        }
        if !likely_pixel_space {
            // Minor out-of-grid events happen at viewport edges in some terminals.
            // Clamp to valid cell coordinates but avoid arming sticky pixel mode.
            if outside_grid {
                mouse.x = mouse.x.min(self.width.saturating_sub(1));
                mouse.y = mouse.y.min(self.height.saturating_sub(1));
            }
            return Event::Mouse(mouse);
        }

        if self.width == 0 || self.height == 0 {
            return Event::Mouse(mouse);
        }
        if self.pixel_width > 0 && self.pixel_height > 0 {
            mouse.x = Self::scale_mouse_coord(mouse.x, self.width, self.pixel_width);
            mouse.y = Self::scale_mouse_coord(mouse.y, self.height, self.pixel_height);
        } else {
            // Fallback when winsize pixel dimensions are unavailable:
            // seed with conservative per-cell estimates so the first event does
            // not collapse toward viewport edges, then expand from observations.
            if self.inferred_pixel_width == 0 {
                self.inferred_pixel_width = self
                    .width
                    .saturating_mul(INFERRED_PIXEL_WIDTH_PER_CELL)
                    .max(self.width);
            }
            if self.inferred_pixel_height == 0 {
                self.inferred_pixel_height = self
                    .height
                    .saturating_mul(INFERRED_PIXEL_HEIGHT_PER_CELL)
                    .max(self.height);
            }
            self.inferred_pixel_width = self
                .inferred_pixel_width
                .max(mouse.x.saturating_add(1))
                .max(self.width);
            self.inferred_pixel_height = self
                .inferred_pixel_height
                .max(mouse.y.saturating_add(1))
                .max(self.height);

            mouse.x =
                Self::scale_mouse_coord(mouse.x, self.width, self.inferred_pixel_width.max(1));
            mouse.y =
                Self::scale_mouse_coord(mouse.y, self.height, self.inferred_pixel_height.max(1));
        }
        Event::Mouse(mouse)
    }

    /// The cell holding pixel `coord`, with `pixels` spread evenly over
    /// `cells`: `coord * cells / pixels`.
    ///
    /// This used `(cells - 1) / (pixels - 1)`, which lines up only the first
    /// and last pixel. With 10-pixel cells, pixel 10 (cell 1's first) came
    /// out as cell 0, and toward the right edge most clicks landed a cell
    /// early.
    #[inline]
    fn scale_mouse_coord(coord: u16, cells: u16, pixels: u16) -> u16 {
        if cells <= 1 {
            return 0;
        }
        if pixels <= 1 {
            return coord.min(cells - 1);
        }
        let scaled = u32::from(coord) * u32::from(cells) / u32::from(pixels);
        u16::try_from(scaled).unwrap_or(u16::MAX).min(cells - 1)
    }

    #[cfg(unix)]
    fn query_tty_winsize(&self) -> Option<rustix::termios::Winsize> {
        if !self.live {
            return None;
        }
        let tty = self.tty_reader.as_ref()?;
        rustix::termios::tcgetwinsize(tty).ok()
    }

    #[cfg(unix)]
    fn query_tty_size(&self) -> Option<(u16, u16)> {
        let ws = self.query_tty_winsize()?;
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return None;
        }
        Some((ws.ws_col, ws.ws_row))
    }

    #[cfg(unix)]
    fn drain_resize_wake_bytes(&mut self) -> bool {
        let Some(reader) = self.resize_reader.as_mut() else {
            return false;
        };
        let mut any = false;
        let mut retire_reader = false;
        let mut buf = [0u8; 64];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    retire_reader = true;
                    break;
                }
                Ok(_) => any = true,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    retire_reader = true;
                    break;
                }
            }
        }
        if retire_reader {
            self.resize_reader = None;
        }
        any
    }

    #[cfg(unix)]
    fn drain_resize_notifications(&mut self) {
        if !self.live {
            return;
        }
        // Drain all pending SIGWINCH notifications, coalescing into a single
        // resize query (the authoritative size comes from ioctl, not the signal).
        let got_resize = self.drain_resize_wake_bytes();
        if got_resize && let Some(ws) = self.query_tty_winsize() {
            self.pixel_width = ws.ws_xpixel;
            self.pixel_height = ws.ws_ypixel;
            if ws.ws_col > 0 && ws.ws_row > 0 {
                self.push_resize(ws.ws_col, ws.ws_row);
            }
        }
    }

    /// Read available bytes from the tty reader and feed them to the parser.
    fn drain_available_bytes(&mut self) -> io::Result<()> {
        if self.tty_reader.is_none() {
            return Ok(());
        }
        let mut buf = [0u8; READ_BUFFER_BYTES];
        let mut drained_bytes = 0usize;
        let mut parsed_events = Vec::new();
        loop {
            let read_result = {
                let Some(tty) = self.tty_reader.as_mut() else {
                    return Ok(());
                };
                tty.read(&mut buf)
            };
            match read_result {
                Ok(0) => {
                    // Treat EOF as terminal source exhaustion. Keeping the fd alive
                    // after a hangup causes future timed polls to wake immediately
                    // on POLLHUP and spin until the outer deadline.
                    self.tty_reader = None;
                    self.reader_nonblocking = false;
                    return Ok(());
                }
                Ok(n) => {
                    self.last_input_byte_at = Some(Instant::now());
                    parsed_events.clear();
                    self.parser
                        .parse_with(&buf[..n], |event| parsed_events.push(event));
                    if let Some(ref mut trace) = self.input_trace {
                        trace.record(&buf[..n], &parsed_events);
                    }
                    for event in parsed_events.drain(..) {
                        let normalized = self.normalize_event(event);
                        self.push_event_coalescing(normalized);
                    }
                    drained_bytes = drained_bytes.saturating_add(n);
                    if !self.reader_nonblocking {
                        return Ok(());
                    }
                    if drained_bytes >= MAX_DRAIN_BYTES_PER_POLL {
                        return Ok(());
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Push an event into the queue, coalescing the hottest high-volume event types.
    ///
    /// Some terminals can emit a very high rate of `Moved` events
    /// (trackpad jitter, hover streams). Coalescing consecutive move events keeps
    /// the queue bounded and prevents input storms from starving the render loop.
    fn push_event_coalescing(&mut self, event: Event) {
        if let Event::Mouse(m) = event
            && matches!(m.kind, MouseEventKind::Moved)
            && matches!(
                self.event_queue.back(),
                Some(Event::Mouse(prev)) if matches!(prev.kind, MouseEventKind::Moved)
            )
        {
            let _ = self.event_queue.pop_back();
        }
        self.event_queue.push_back(event);
    }

    #[inline]
    fn parser_timeout_event_if_due(&mut self, now: Instant) -> Option<Event> {
        if !self.parser.has_pending_timeout_state() {
            return None;
        }
        if let Some(last) = self.last_input_byte_at
            && now.saturating_duration_since(last) < PARSER_TIMEOUT_GRACE
        {
            return None;
        }
        let event = self.parser.timeout();
        if event.is_some() {
            self.last_input_byte_at = None;
        }
        event
    }

    #[inline]
    fn parser_timeout_wait_budget(&self) -> Option<Duration> {
        if !self.parser.has_pending_timeout_state() {
            return None;
        }
        Some(
            self.last_input_byte_at
                .map(|last| PARSER_TIMEOUT_GRACE.saturating_sub(last.elapsed()))
                .unwrap_or(Duration::ZERO),
        )
    }

    /// Give immediately ready bytes one last chance to complete an ambiguous
    /// parser state before synthesizing a timeout event.
    fn drain_ready_bytes_before_parser_timeout(&mut self) -> io::Result<bool> {
        if self.tty_reader.is_none() {
            return Ok(false);
        }
        if self.reader_nonblocking {
            self.drain_available_bytes()?;
        } else {
            let _ = self.poll_tty(Duration::ZERO)?;
        }
        Ok(!self.event_queue.is_empty())
    }

    /// Poll the tty fd for available data using `poll(2)`.
    ///
    /// On macOS, `poll(2)` on `/dev/tty` always returns `POLLNVAL` even though
    /// the fd is valid for `open()`, `fcntl()`, and nonblocking `read()`.  When
    /// this happens we fall back to a nonblocking `drain_available_bytes()` call
    /// (if the reader is in nonblocking mode) so that input still works.  A
    /// short backoff sleep prevents a tight spin loop.
    #[cfg(unix)]
    fn poll_tty(&mut self, timeout: Duration) -> io::Result<bool> {
        use std::os::fd::AsFd;

        /// Backoff sleep when poll(2) reports POLLNVAL (macOS /dev/tty).
        const TTY_UNAVAILABLE_BACKOFF: Duration = Duration::from_millis(8);

        let (tty_ready, tty_unavailable, resize_ready) = {
            let Some(ref tty) = self.tty_reader else {
                return Ok(false);
            };
            let mut poll_fds = Vec::with_capacity(2);
            poll_fds.push(nix::poll::PollFd::new(
                tty.as_fd(),
                nix::poll::PollFlags::POLLIN,
            ));
            let resize_index = if let Some(ref resize_reader) = self.resize_reader {
                poll_fds.push(nix::poll::PollFd::new(
                    resize_reader.as_fd(),
                    nix::poll::PollFlags::POLLIN,
                ));
                Some(1usize)
            } else {
                None
            };
            // Use i32 for timeout to allow values > 65s (up to ~24 days).
            // poll(2) takes milliseconds as a signed int.
            let timeout_ms: i32 = timeout.as_millis().try_into().unwrap_or(i32::MAX);
            let _ = match nix::poll::poll(
                &mut poll_fds,
                nix::poll::PollTimeout::try_from(timeout_ms).unwrap_or(nix::poll::PollTimeout::MAX),
            ) {
                Ok(n) => n,
                Err(nix::errno::Errno::EINTR) => return Ok(false),
                Err(e) => return Err(io::Error::other(e)),
            };
            let tty_revents = poll_fds.first().and_then(nix::poll::PollFd::revents);
            let tty_ready = tty_revents.is_some_and(|revents| {
                revents.intersects(
                    nix::poll::PollFlags::POLLIN
                        | nix::poll::PollFlags::POLLERR
                        | nix::poll::PollFlags::POLLHUP,
                )
            });
            let tty_unavailable = tty_revents
                .is_some_and(|revents| revents.intersects(nix::poll::PollFlags::POLLNVAL));
            let resize_ready = resize_index
                .and_then(|idx| poll_fds.get(idx))
                .and_then(nix::poll::PollFd::revents)
                .is_some_and(|revents| {
                    revents.intersects(
                        nix::poll::PollFlags::POLLIN
                            | nix::poll::PollFlags::POLLERR
                            | nix::poll::PollFlags::POLLHUP,
                    )
                });
            (tty_ready, tty_unavailable, resize_ready)
        };
        if tty_ready {
            self.drain_available_bytes()?;
        } else if tty_unavailable {
            // macOS: /dev/tty doesn't support poll(2) and always returns
            // POLLNVAL, but the fd is valid for nonblocking reads.
            if self.reader_nonblocking {
                self.drain_available_bytes()?;
            }
            // Always process resize even on the POLLNVAL path — the resize
            // fd is a UnixStream, not /dev/tty, so poll(2) works fine on it.
            if resize_ready {
                self.drain_resize_notifications();
            }
            if !self.event_queue.is_empty() {
                return Ok(true);
            }
            if timeout != Duration::ZERO {
                std::thread::sleep(timeout.min(TTY_UNAVAILABLE_BACKOFF));
            }
            return Ok(!self.event_queue.is_empty());
        }
        if resize_ready {
            self.drain_resize_notifications();
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
        capabilities: TerminalCapabilities,
        writer: &mut impl Write,
    ) -> io::Result<()> {
        let mouse_enable_seq = mouse_enable_sequence_for_capabilities(capabilities);
        let mouse_disable_seq = mouse_disable_sequence_for_capabilities(capabilities);
        Self::write_feature_delta_with_mouse(
            current,
            new,
            mouse_enable_seq,
            mouse_disable_seq,
            writer,
        )
    }

    fn write_feature_delta_with_mouse(
        current: &BackendFeatures,
        new: &BackendFeatures,
        mouse_enable_seq: &[u8],
        mouse_disable_seq: &[u8],
        writer: &mut impl Write,
    ) -> io::Result<()> {
        if new.mouse_capture != current.mouse_capture {
            writer.write_all(if new.mouse_capture {
                mouse_enable_seq
            } else {
                mouse_disable_seq
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
                // This push owes a pop, so release the one-outstanding-push
                // latch. It is not one pop per process: leaving it claimed
                // from an earlier session's best-effort teardown suppressed
                // the pop in `write_cleanup_sequence_policy_with_mouse` for
                // every session after it, leaving the terminal in
                // enhanced-key mode once the program was gone.
                KittyPopLatch::release();
                KITTY_KEYBOARD_ENABLE
            } else {
                KITTY_KEYBOARD_DISABLE
            })?;
        }
        Ok(())
    }

    /// Disable all active features, writing escape sequences to `writer`.
    #[allow(dead_code)]
    fn disable_all(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let off = BackendFeatures::default();
        Self::write_feature_delta(&self.features, &off, self.capabilities, writer)?;
        self.apply_feature_state(off);
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
        let effective_features = self.sanitize_features(features);
        if self.live {
            let mut stdout = io::stdout();
            if let Err(err) = Self::write_feature_delta(
                &self.features,
                &effective_features,
                self.capabilities,
                &mut stdout,
            )
            .and_then(|_| stdout.flush())
            {
                // A failed write can still partially apply terminal modes.
                // Track a conservative superset so drop-time cleanup disables
                // anything that might have been enabled before the error.
                self.apply_feature_state(conservative_feature_union(
                    self.features,
                    effective_features,
                ));
                return Err(err);
            }
        }
        self.apply_feature_state(effective_features);
        Ok(())
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, Self::Error> {
        #[cfg(unix)]
        self.drain_resize_notifications();

        // If we already have buffered events, return immediately.
        if !self.event_queue.is_empty() {
            return Ok(true);
        }

        if timeout == Duration::ZERO {
            let ready = self.poll_tty(Duration::ZERO)?;
            if !ready && self.drain_ready_bytes_before_parser_timeout()? {
                return Ok(true);
            }
            if !ready && let Some(event) = self.parser_timeout_event_if_due(Instant::now()) {
                self.event_queue.push_back(event);
                return Ok(true);
            }
            return Ok(!self.event_queue.is_empty());
        }

        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(std::time::Instant::now);

        loop {
            if !self.event_queue.is_empty() {
                return Ok(true);
            }

            if self.tty_reader.is_none() && !self.parser.has_pending_timeout_state() {
                return Ok(false);
            }

            if self.parser.has_pending_timeout_state() {
                if self.drain_ready_bytes_before_parser_timeout()? {
                    return Ok(true);
                }
                if let Some(event) = self.parser_timeout_event_if_due(Instant::now()) {
                    self.event_queue.push_back(event);
                    return Ok(true);
                }
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }

            let mut poll_for = deadline.saturating_duration_since(now);
            if let Some(parser_wait_budget) = self.parser_timeout_wait_budget() {
                poll_for = poll_for.min(parser_wait_budget);
            }

            let _ = self.poll_tty(poll_for)?;
            #[cfg(unix)]
            self.drain_resize_notifications();
        }
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        if let Some(event) = self.event_queue.pop_front() {
            return Ok(Some(event));
        }

        #[cfg(unix)]
        {
            self.drain_resize_notifications();
            if let Some(event) = self.event_queue.pop_front() {
                return Ok(Some(event));
            }
        }

        // Opportunistically drain any newly-arrived bytes when the reader is nonblocking.
        //
        // This reduces poll(2) syscalls in bursty workloads by allowing the consumer's
        // `while let Some(e) = read_event()` drain loop to pick up additional input
        // without requiring another `poll_event()` round-trip.
        if self.drain_ready_bytes_before_parser_timeout()?
            && let Some(event) = self.event_queue.pop_front()
        {
            return Ok(Some(event));
        }

        Ok(self.parser_timeout_event_if_due(Instant::now()))
    }
}

// ── Backend ──────────────────────────────────────────────────────────────

/// Native Unix terminal backend.
///
/// Combines `TtyClock` and `TtyEventSource` into a single `Backend` implementation
/// that the ftui runtime can drive.
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
    // 3. alt_screen_active (tracked for cleanup)
    // 4. raw_mode — MUST be last: termios is restored after escape sequences
    clock: TtyClock,
    events: TtyEventSource,
    alt_screen_active: bool,
    #[cfg(unix)]
    signal_interception_active: bool,
    /// What `suspend` turned off, for `resume` to turn back on.
    #[cfg(unix)]
    suspended: Option<SuspendedSession>,
    #[cfg(unix)]
    raw_mode: Option<RawModeGuard>,
}

/// Terminal state a job-control suspend released.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct SuspendedSession {
    features: BackendFeatures,
    alt_screen: bool,
}

impl TtyBackend {
    /// Create a headless backend (no terminal I/O). Useful for testing.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::new(width, height),
            alt_screen_active: false,
            #[cfg(unix)]
            signal_interception_active: false,
            #[cfg(unix)]
            suspended: None,
            #[cfg(unix)]
            raw_mode: None,
        }
    }

    /// Create a headless backend with explicit capabilities.
    #[must_use]
    pub fn with_capabilities(width: u16, height: u16, capabilities: TerminalCapabilities) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::with_capabilities(width, height, capabilities),
            alt_screen_active: false,
            #[cfg(unix)]
            signal_interception_active: false,
            #[cfg(unix)]
            suspended: None,
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
        install_abort_panic_hook();
        let mut signal_guard = SignalInterceptGuard::new(options.intercept_signals);
        let capabilities = TerminalCapabilities::with_overrides();
        let requested_features = options.features;
        let effective_features = sanitize_feature_request(requested_features, capabilities);

        let mut stdout = io::stdout();
        let mut alt_screen_active = false;

        // Enable initial features.
        let mut events = TtyEventSource::live(width, height, capabilities)?;
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
                &effective_features,
                capabilities,
                &mut stdout,
            )?;

            stdout.flush()?;
            Ok(())
        })();

        if let Err(err) = setup {
            // Best-effort cleanup: we may have partially enabled features or entered alt screen.
            //
            // No synchronized-output block has been opened during setup, so avoid
            // emitting a standalone DEC ?2026l on this path.
            let mouse_disable_seq = mouse_disable_sequence_for_capabilities(capabilities);
            let _ = write_cleanup_sequence_policy_with_mouse(
                &effective_features,
                options.alternate_screen,
                false,
                mouse_disable_seq,
                &mut stdout,
            );
            let _ = stdout.flush();
            return Err(err);
        }

        events.apply_feature_state(effective_features);

        #[cfg(unix)]
        TTY_SESSION_ACTIVE.store(true, Ordering::SeqCst);

        Ok(Self {
            clock: TtyClock::new(),
            events,
            alt_screen_active,
            signal_interception_active: signal_guard.disarm(),
            suspended: None,
            raw_mode: Some(raw_mode),
        })
    }

    /// Give back this backend's slot in [`LIVE_SIGNAL_INTERCEPT_SESSIONS`],
    /// if it holds one. Idempotent.
    ///
    /// Separate from the teardown-sequence path on purpose. That path is
    /// gated on `TTY_SESSION_ACTIVE`, which answers "have the cleanup bytes
    /// already gone out?" - and `best_effort_termination_cleanup` answers it
    /// `false` for us on the way past, from both the panic hook and the
    /// termination-signal thread. A slot must come back on every drop, not
    /// only on the drops that still had bytes left to write.
    #[cfg(unix)]
    fn release_signal_interception(&mut self) {
        if self.signal_interception_active {
            LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_sub(1, Ordering::SeqCst);
            self.signal_interception_active = false;
        }
    }

    /// Hand the terminal back for a job-control stop: the teardown `drop`
    /// writes (input modes off, cursor shown, alt screen left), then the
    /// original termios, keeping the guard for [`resume_session`](Self::resume_session).
    ///
    /// Returns `false` for a headless backend. Idempotent while suspended.
    #[cfg(unix)]
    fn suspend_session(&mut self) -> io::Result<bool> {
        let Some(raw_mode) = self.raw_mode.as_ref() else {
            return Ok(false);
        };
        if self.suspended.is_some() {
            return Ok(true);
        }
        let session = SuspendedSession {
            features: self.events.features(),
            alt_screen: self.alt_screen_active,
        };
        let mut stdout = io::stdout();
        let mouse_disable_seq = mouse_disable_sequence_for_capabilities(self.events.capabilities);
        // Same sequence and order as `drop`, and for the same reasons: no
        // synchronized-output end (the presenter owns that block), cursor
        // shown and alt screen left while the terminal still reads escapes.
        write_cleanup_sequence_policy_with_mouse(
            &session.features,
            session.alt_screen,
            false,
            mouse_disable_seq,
            &mut stdout,
        )?;
        stdout.flush()?;
        // Recorded before termios, so a failure below still leaves `resume`
        // (or `drop`) with an accurate picture of what is switched off.
        self.suspended = Some(session);
        self.alt_screen_active = false;
        self.events.apply_feature_state(BackendFeatures::default());
        raw_mode.restore_original()?;
        Ok(true)
    }

    /// Undo [`suspend_session`](Self::suspend_session): raw mode, then the
    /// alternate screen (cleared: its contents are gone), then input modes,
    /// the reverse of the teardown order.
    #[cfg(unix)]
    fn resume_session(&mut self) -> io::Result<()> {
        let Some(session) = self.suspended.take() else {
            return Ok(());
        };
        let Some(raw_mode) = self.raw_mode.as_ref() else {
            return Ok(());
        };
        raw_mode.reenter_raw()?;
        let mut stdout = io::stdout();
        if session.alt_screen {
            stdout.write_all(ALT_SCREEN_ENTER)?;
            stdout.write_all(CLEAR_SCREEN)?;
            stdout.write_all(CURSOR_HOME)?;
            self.alt_screen_active = true;
        }
        TtyEventSource::write_feature_delta(
            &BackendFeatures::default(),
            &session.features,
            self.events.capabilities,
            &mut stdout,
        )?;
        stdout.flush()?;
        self.events.apply_feature_state(session.features);
        Ok(())
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
        // Unconditionally, and before anything that can return early: the
        // signal-interception slot is this backend's to give back whether or
        // not the teardown bytes are still owed.
        //
        // Leaked above zero, the counter tells the signal thread a live
        // session still wants to handle SIGINT itself, long after that session
        // is gone. The thread then records a pending signal nobody will ever
        // acknowledge and waits out the whole `SIGNAL_SHUTDOWN_GRACE` before
        // exiting - so one stuck slot costs every later Ctrl-C two seconds.
        #[cfg(unix)]
        self.release_signal_interception();

        // Only run cleanup if we have an active session.
        #[cfg(unix)]
        if self.raw_mode.is_some() {
            if !TTY_SESSION_ACTIVE.swap(false, Ordering::SeqCst) {
                return;
            }
            let mut stdout = io::stdout();
            let mouse_disable_seq =
                mouse_disable_sequence_for_capabilities(self.events.capabilities);
            // No standalone `?2026l`: this backend opens no synchronized-output
            // block. The presenter (`TerminalWriter`) owns each frame's block
            // and closes an open one in its own drop, so a teardown sync end
            // was always unpaired - under a multiplexer identity it is also
            // forbidden (vfx_shape3d_wezterm_mux_policy_omits_sync_output_sequences),
            // and elsewhere it broke the begin/end pairing that
            // editor_clipboard_payload_cap asserts. The pre-refactor Drop
            // emitted no sync end at all.
            let _ = write_cleanup_sequence_policy_with_mouse(
                &self.events.features(),
                self.alt_screen_active,
                false,
                mouse_disable_seq,
                &mut stdout,
            );
            self.alt_screen_active = false;
            self.events.apply_feature_state(BackendFeatures::default());

            // Flush everything before RawModeGuard restores termios.
            let _ = stdout.flush();

            // RawModeGuard::drop() runs after this, restoring original termios.
        }
    }
}

/// Allow `TtyBackend` to be used directly as a `BackendEventSource` in
/// `Program<M, TtyBackend, P>`. Delegates to the inner `TtyEventSource`.
/// This is the primary integration point: the runtime owns a `TtyBackend`
/// as its event source, which also provides RAII terminal cleanup on drop.
impl BackendEventSource for TtyBackend {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), io::Error> {
        self.events.size()
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), io::Error> {
        self.events.set_features(features)
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, io::Error> {
        self.events.poll_event(timeout)
    }

    fn read_event(&mut self) -> Result<Option<Event>, io::Error> {
        self.events.read_event()
    }

    fn supports_suspend(&self) -> bool {
        self.is_live()
    }

    fn suspend(&mut self) -> Result<bool, io::Error> {
        #[cfg(unix)]
        {
            self.suspend_session()
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    fn resume(&mut self) -> Result<(), io::Error> {
        #[cfg(unix)]
        {
            self.resume_session()
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

impl Backend for TtyBackend {
    type Error = io::Error;
    type Clock = TtyClock;
    type Events = TtyEventSource;

    fn clock(&self) -> &Self::Clock {
        &self.clock
    }

    fn events(&mut self) -> &mut Self::Events {
        &mut self.events
    }
}

// ── Utility: write cleanup sequence to a byte buffer (for testing) ───────

/// Write the full cleanup sequence for the given feature state to `writer`.
///
/// This is useful for verifying cleanup in PTY tests without needing
/// a real terminal session. By default this omits DEC ?2026l because no
/// synchronized-output ownership is implied by this utility API.
pub fn write_cleanup_sequence(
    features: &BackendFeatures,
    alt_screen: bool,
    writer: &mut impl Write,
) -> io::Result<()> {
    write_cleanup_sequence_policy(features, alt_screen, false, writer)
}

/// Write cleanup with an explicit DEC ?2026l prefix.
///
/// Use this only when the caller owns a matching synchronized-output begin.
pub fn write_cleanup_sequence_with_sync_end(
    features: &BackendFeatures,
    alt_screen: bool,
    writer: &mut impl Write,
) -> io::Result<()> {
    write_cleanup_sequence_policy(features, alt_screen, true, writer)
}

fn write_cleanup_sequence_policy(
    features: &BackendFeatures,
    alt_screen: bool,
    emit_sync_end: bool,
    writer: &mut impl Write,
) -> io::Result<()> {
    write_cleanup_sequence_policy_with_mouse(
        features,
        alt_screen,
        emit_sync_end,
        MOUSE_DISABLE,
        writer,
    )
}

fn write_cleanup_sequence_policy_with_mouse(
    features: &BackendFeatures,
    alt_screen: bool,
    emit_sync_end: bool,
    mouse_disable_seq: &'static [u8],
    writer: &mut impl Write,
) -> io::Result<()> {
    let pop_kitty = if features.kitty_keyboard {
        !KittyPopLatch::is_claimed()
    } else {
        false
    };
    let plan = TeardownPlan {
        emit_sync_end,
        reset_scroll_region: true,
        reset_style: true,
        pop_kitty_keyboard: pop_kitty,
        disable_focus: features.focus_events,
        disable_paste: features.bracketed_paste,
        disable_mouse: features.mouse_capture,
        mouse_mux_safe: false,
        mouse_disable_override: Some(mouse_disable_seq),
        show_cursor: true,
        leave_alt_screen: alt_screen,
    };
    plan.write_for_backend(writer, "tty")
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::session_teardown::seq::*;

    /// Shared lock for all tests that depend on the process-global `KittyPopLatch`.
    ///
    /// `write_cleanup_sequence*` consults `KittyPopLatch::is_claimed()` so that only
    /// one component emits the kitty keyboard pop per process. Tests that assert on
    /// that pop must reset the latch first and must not race each other for it.
    /// Mirrors the `gpu_test_lock` pattern in `ftui-extras`.
    fn kitty_latch_test_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static KITTY_LATCH_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        KITTY_LATCH_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
    fn headless_backend_has_no_session_to_suspend() {
        // The runtime claims the stop signals only for a backend that can
        // hand a terminal back; a headless one must say it cannot, or a stop
        // would strand whatever terminal the process does have.
        let mut backend = TtyBackend::new(80, 24);
        assert!(!backend.supports_suspend());
        assert!(!backend.suspend().unwrap());
        backend.resume().unwrap();
        assert!(!backend.is_live());
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
        let codes: Vec<KeyCode> = std::iter::from_fn(|| src.read_event().unwrap())
            .map(|e| match e {
                Event::Key(KeyEvent { code, .. }) => Ok(code),
                other => Err(other),
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
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
    fn pipe_mouse_x10_click_when_mouse_capture_enabled() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.set_features(BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        })
        .unwrap();

        // X10 mouse: left click at (10, 20) in 1-indexed protocol coordinates.
        // CSI M Cb Cx Cy, with each byte encoded as value + 32 (or +33 for x/y).
        writer.write_all(&[0x1B, b'[', b'M', 32, 42, 52]).unwrap();
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
    fn pipe_mouse_x10_not_decoded_when_mouse_capture_disabled() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.set_features(BackendFeatures::default()).unwrap();

        // Same X10 sequence as above; with mouse capture disabled, this should
        // not be interpreted as a mouse event.
        writer.write_all(&[0x1B, b'[', b'M', 32, 42, 52]).unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let e = src.read_event().unwrap().unwrap();
        assert!(matches!(
            e,
            Event::Key(KeyEvent {
                code: KeyCode::Char(' '),
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pipe_mouse_legacy_1015_click_when_mouse_capture_enabled() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.set_features(BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        })
        .unwrap();

        // Legacy xterm/rxvt 1015 mouse: CSI Cb;Cx;Cy M
        writer.write_all(b"\x1b[0;10;20M").unwrap();
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
    fn pipe_mouse_legacy_1015_not_decoded_when_mouse_capture_disabled() {
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.set_features(BackendFeatures::default()).unwrap();

        writer.write_all(b"\x1b[0;10;20M").unwrap();
        assert!(!src.poll_event(Duration::from_millis(25)).unwrap());
        assert!(src.read_event().unwrap().is_none());
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

    #[cfg(unix)]
    #[test]
    fn pipe_large_ascii_burst_roundtrips() {
        use ftui_core::event::{KeyCode, KeyEvent};

        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        let payload = vec![b'a'; 4 * 1024 * 1024];
        let expected_len = payload.len();
        let writer_thread = std::thread::spawn(move || writer.write_all(&payload));

        let mut count = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while count < expected_len {
            if !src.poll_event(Duration::from_millis(100)).unwrap() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for burst events: received {count} / {expected_len}"
                );
                continue;
            }
            while let Some(event) = src.read_event().unwrap() {
                match event {
                    Event::Key(KeyEvent {
                        code: KeyCode::Char('a'),
                        ..
                    }) => count += 1,
                    other => panic!("unexpected event in ascii burst test: {other:?}"),
                }
            }
        }
        writer_thread.join().unwrap().unwrap();

        assert_eq!(count, expected_len, "all bytes should decode to key events");
    }

    // ── Edge-case input parser tests ─────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn truncated_csi_followed_by_valid_input() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Write an incomplete CSI sequence followed by a valid character.
        // The incomplete `\x1b[` should be buffered; when `a` arrives
        // (not a valid CSI final byte when directly after `[`), the parser
        // should eventually recover. We follow with a clear valid sequence.
        writer.write_all(b"\x1b[").unwrap();
        // Give the poll a chance to consume the partial sequence.
        let _ = src.poll_event(Duration::from_millis(50));
        // Now send a valid key to force the parser forward.
        writer.write_all(b"\x1b[Ax").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        // Drain all events and verify we get at least the valid ones.
        let mut events = Vec::new();
        while let Some(e) = src.read_event().unwrap() {
            events.push(e);
        }
        // The Up arrow (\x1b[A) and the 'x' key should both be parsed.
        let has_up = events.iter().any(|e| {
            matches!(
                e,
                Event::Key(KeyEvent {
                    code: KeyCode::Up,
                    ..
                })
            )
        });
        let has_x = events.iter().any(|e| {
            matches!(
                e,
                Event::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    modifiers: Modifiers::NONE,
                    kind: KeyEventKind::Press,
                })
            )
        });
        assert!(
            has_up,
            "should parse Up arrow after partial CSI: {events:?}"
        );
        assert!(has_x, "should parse 'x' after recovery: {events:?}");
    }

    #[cfg(unix)]
    #[test]
    fn unknown_csi_sequence_does_not_block_parser() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // \x1b[999~ is an unknown tilde-code; the parser should silently
        // drop it and still parse the subsequent 'z' key event.
        writer.write_all(b"\x1b[999~z").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let mut events = Vec::new();
        while let Some(e) = src.read_event().unwrap() {
            events.push(e);
        }
        let has_z = events.iter().any(|e| {
            matches!(
                e,
                Event::Key(KeyEvent {
                    code: KeyCode::Char('z'),
                    modifiers: Modifiers::NONE,
                    kind: KeyEventKind::Press,
                })
            )
        });
        assert!(
            has_z,
            "valid key after unknown CSI must be parsed: {events:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn eof_on_pipe_does_not_panic() {
        let (reader, writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Close the writer end immediately to simulate EOF.
        drop(writer);
        // poll_event should return false (no data) without panicking.
        let result = src.poll_event(Duration::from_millis(50));
        assert!(result.is_ok(), "poll_event after EOF should not error");
        assert!(
            src.tty_reader.is_none(),
            "EOF should retire the exhausted reader"
        );
        // read_event should also return None cleanly.
        let event = src.read_event().unwrap();
        assert!(event.is_none(), "read_event after EOF should be None");
    }

    #[cfg(unix)]
    #[test]
    fn eof_disables_reader_for_future_polls() {
        let (reader, writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        drop(writer);

        assert!(!src.poll_event(Duration::from_millis(20)).unwrap());
        assert!(src.tty_reader.is_none(), "EOF should clear the reader");

        let start = Instant::now();
        assert!(!src.poll_event(Duration::from_millis(200)).unwrap());
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "polls after EOF should return immediately once the reader is retired"
        );
    }

    #[cfg(unix)]
    #[test]
    fn interleaved_invalid_and_valid_sequences() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Mix of: invalid UTF-8 lead byte, valid 'a', unknown CSI, valid 'b',
        // bare ESC followed by valid char, valid 'c'.
        writer.write_all(b"\xC0a\x1b[999~b\x1b c").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let mut key_chars = Vec::new();
        while let Some(e) = src.read_event().unwrap() {
            if let Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                ..
            }) = e
            {
                key_chars.push(ch);
            }
        }
        // 'a', 'b', and 'c' must all appear (possibly with Alt modifier for 'c'
        // since \x1b followed by space+c could parse as Alt+Space then 'c').
        assert!(
            key_chars.contains(&'a'),
            "should parse 'a' amid invalid input: {key_chars:?}"
        );
        assert!(
            key_chars.contains(&'b'),
            "should parse 'b' amid invalid input: {key_chars:?}"
        );
        assert!(
            key_chars.contains(&'c'),
            "should parse 'c' amid invalid input: {key_chars:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn split_escape_sequence_across_writes() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Write the escape sequence for Down arrow (\x1b[B) in two separate writes.
        writer.write_all(b"\x1b").unwrap();
        // First poll: the lone ESC may or may not produce an event depending
        // on whether the parser waits for more bytes.
        let _ = src.poll_event(Duration::from_millis(30));
        // Complete the sequence.
        writer.write_all(b"[B").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let mut events = Vec::new();
        while let Some(e) = src.read_event().unwrap() {
            events.push(e);
        }
        let has_down = events.iter().any(|e| {
            matches!(
                e,
                Event::Key(KeyEvent {
                    code: KeyCode::Down,
                    ..
                })
            )
        });
        assert!(
            has_down,
            "Down arrow split across writes should be parsed: {events:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn poll_with_zero_timeout_returns_false_on_empty_pipe() {
        let (reader, _writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Zero-timeout poll with no data should return false immediately.
        let ready = src.poll_event(Duration::ZERO).unwrap();
        assert!(!ready, "empty pipe with zero timeout should not be ready");
    }

    #[cfg(unix)]
    #[test]
    fn zero_timeout_poll_resolves_pending_escape_after_grace() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);

        // Begin an ambiguous escape sequence with a lone ESC byte.
        writer.write_all(b"\x1b").unwrap();

        // Immediate zero-timeout poll should not resolve it yet.
        let ready = src.poll_event(Duration::ZERO).unwrap();
        assert!(!ready, "pending ESC should wait for timeout grace");

        // After grace elapses, a zero-timeout poll should emit Escape.
        std::thread::sleep(PARSER_TIMEOUT_GRACE + Duration::from_millis(10));
        let ready = src.poll_event(Duration::ZERO).unwrap();
        assert!(ready, "zero-timeout poll should resolve overdue ESC");

        let event = src.read_event().unwrap();
        assert!(matches!(
            event,
            Some(Event::Key(KeyEvent {
                code: KeyCode::Escape,
                ..
            }))
        ));
    }

    #[test]
    fn escape_grace_boundary_and_fragment_completion_are_deterministic() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let start = Instant::now();
        let mut src = TtyEventSource::new(80, 24);
        assert!(src.parser.parse(b"\x1b").is_empty());
        src.last_input_byte_at = Some(start);
        assert!(src.parser_timeout_event_if_due(start).is_none());
        assert!(
            src.parser_timeout_event_if_due(start + PARSER_TIMEOUT_GRACE - Duration::from_nanos(1))
                .is_none()
        );
        assert_eq!(
            src.parser_timeout_event_if_due(start + PARSER_TIMEOUT_GRACE),
            Some(Event::Key(KeyEvent::new(KeyCode::Escape)))
        );
        assert!(
            src.parser_timeout_event_if_due(start + PARSER_TIMEOUT_GRACE * 2)
                .is_none(),
            "expired Escape must be delivered only once"
        );

        // A fragmented CSI completes during grace; it must not also emit Escape.
        assert!(src.parser.parse(b"\x1b").is_empty());
        src.last_input_byte_at = Some(start);
        assert!(
            src.parser_timeout_event_if_due(start + PARSER_TIMEOUT_GRACE / 2)
                .is_none()
        );
        assert_eq!(
            src.parser.parse(b"[C"),
            vec![Event::Key(KeyEvent::new(KeyCode::Right))]
        );
        assert!(
            src.parser_timeout_event_if_due(start + PARSER_TIMEOUT_GRACE)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_poll_waits_for_pending_escape_to_become_ready() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);

        writer.write_all(b"\x1b").unwrap();

        let ready = src.poll_event(Duration::from_millis(200)).unwrap();
        assert!(
            ready,
            "poll should wait for pending ESC to resolve within timeout"
        );
        assert!(matches!(
            src.read_event().unwrap(),
            Some(Event::Key(KeyEvent {
                code: KeyCode::Escape,
                ..
            }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resize_aware_poll_resolves_pending_escape_before_outer_timeout() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let (resize_reader, _resize_writer) = UnixStream::pair().unwrap();
        resize_reader.set_nonblocking(true).unwrap();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.live = true;
        src.resize_reader = Some(resize_reader);

        writer.write_all(b"\x1b").unwrap();

        let start = Instant::now();
        let ready = src.poll_event(Duration::from_millis(250)).unwrap();
        let elapsed = start.elapsed();
        assert!(
            ready,
            "poll should resolve pending ESC while timeout budget remains"
        );
        assert!(
            elapsed < Duration::from_millis(200),
            "pending ESC should resolve near parser grace, not at outer deadline: {elapsed:?}"
        );
        assert!(matches!(
            src.read_event().unwrap(),
            Some(Event::Key(KeyEvent {
                code: KeyCode::Escape,
                ..
            }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resize_wake_bytes_are_drained_and_coalesced() {
        let (resize_reader, mut resize_writer) = UnixStream::pair().unwrap();
        resize_reader.set_nonblocking(true).unwrap();
        resize_writer.set_nonblocking(true).unwrap();

        let mut src = TtyEventSource::new(80, 24);
        src.live = true;
        src.resize_reader = Some(resize_reader);

        resize_writer.write_all(&[1, 1, 1]).unwrap();

        assert!(
            src.drain_resize_wake_bytes(),
            "pending wake bytes should be observed"
        );
        assert!(
            !src.drain_resize_wake_bytes(),
            "draining should coalesce all pending wake bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn speculative_read_resolves_pending_escape_after_grace() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);

        writer.write_all(b"\x1b").unwrap();

        let ready = src.poll_event(Duration::ZERO).unwrap();
        assert!(!ready, "pending ESC should wait for timeout grace");

        std::thread::sleep(PARSER_TIMEOUT_GRACE + Duration::from_millis(10));

        let event = src.read_event().unwrap();
        assert!(matches!(
            event,
            Some(Event::Key(KeyEvent {
                code: KeyCode::Escape,
                ..
            }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn speculative_read_prefers_ready_bytes_over_timeout_resolution_on_blocking_reader() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        src.reader_nonblocking = false;

        writer.write_all(b"\x1b").unwrap();

        let ready = src.poll_event(Duration::ZERO).unwrap();
        assert!(!ready, "pending ESC should wait for timeout grace");

        writer.write_all(b"[B").unwrap();
        std::thread::sleep(PARSER_TIMEOUT_GRACE + Duration::from_millis(10));

        let event = src.read_event().unwrap();
        assert!(matches!(
            event,
            Some(Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn malformed_sgr_mouse_does_not_block() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let (reader, mut writer) = pipe_pair();
        let mut src = TtyEventSource::from_reader(80, 24, reader);
        // Malformed SGR mouse: missing coordinates followed by valid 'q'.
        writer.write_all(b"\x1b[<M q").unwrap();
        assert!(src.poll_event(Duration::from_millis(100)).unwrap());
        let mut events = Vec::new();
        while let Some(e) = src.read_event().unwrap() {
            events.push(e);
        }
        // Parser must recover; 'q' should appear somewhere in the events.
        let has_q = events.iter().any(|e| {
            matches!(
                e,
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                })
            )
        });
        assert!(
            has_q,
            "should parse 'q' after malformed SGR mouse: {events:?}"
        );
    }

    // ── Presenter edge-case tests ────────────────────────────────────

    #[test]
    fn buffer_zero_width_clamped_to_one() {
        let buf = Buffer::new(0, 5);
        assert_eq!(buf.width(), 1);
        assert_eq!(buf.height(), 5);
    }

    #[test]
    fn buffer_zero_height_clamped_to_one() {
        let buf = Buffer::new(5, 0);
        assert_eq!(buf.width(), 5);
        assert_eq!(buf.height(), 1);
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
        TtyEventSource::write_feature_delta(
            &current,
            &new,
            TerminalCapabilities::modern(),
            &mut buf,
        )
        .unwrap();
        assert!(
            buf.windows(MOUSE_ENABLE.len()).any(|w| w == MOUSE_ENABLE),
            "expected mouse enable sequence"
        );
        assert!(
            !buf.windows(b"\x1b[?1003h".len())
                .any(|w| w == b"\x1b[?1003h"),
            "mouse enable should avoid 1003 any-event mode"
        );
        assert!(
            !buf.ends_with(b"\x1b[?1016l"),
            "mouse enable should not end with 1016l (can force X10 fallback on some terminals)"
        );
        let pos_1016l = buf
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mouse enable should clear 1016 before enabling SGR");
        let pos_1006h = buf
            .windows(b"\x1b[?1006h".len())
            .position(|w| w == b"\x1b[?1006h")
            .expect("mouse enable should include 1006 SGR mode");
        assert!(
            pos_1016l < pos_1006h,
            "1016l must be emitted before 1006h to preserve SGR mode on Ghostty-like terminals"
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

    /// A session that enables kitty keyboard pushes onto the stack, so it owes
    /// a pop even if an earlier session's best-effort teardown already claimed
    /// the latch. Enabling therefore releases it; without that, the first
    /// claim in the process suppressed the pop for every session after it.
    #[test]
    fn enabling_kitty_keyboard_releases_the_pop_latch() {
        let _serial = kitty_latch_test_lock();
        use ftui_core::session_teardown::KittyPopLatch;

        // Stand in for an earlier session whose best-effort teardown popped.
        KittyPopLatch::release();
        assert!(KittyPopLatch::try_claim());
        assert!(KittyPopLatch::is_claimed());

        let current = BackendFeatures::default();
        let new = BackendFeatures {
            kitty_keyboard: true,
            ..BackendFeatures::default()
        };
        let mut buf = Vec::new();
        TtyEventSource::write_feature_delta(
            &current,
            &new,
            TerminalCapabilities::modern(),
            &mut buf,
        )
        .unwrap();
        assert!(
            buf.windows(KITTY_KEYBOARD_ENABLE.len())
                .any(|w| w == KITTY_KEYBOARD_ENABLE),
            "the delta should have pushed"
        );
        assert!(
            !KittyPopLatch::is_claimed(),
            "a fresh push owes a fresh pop, so enabling must release the latch"
        );

        // And the teardown for that push is emittable again.
        let mut cleanup = Vec::new();
        write_cleanup_sequence(&new, false, &mut cleanup).unwrap();
        assert!(
            cleanup
                .windows(KITTY_KEYBOARD_DISABLE.len())
                .any(|w| w == KITTY_KEYBOARD_DISABLE),
            "the second session's push must still be popped"
        );
        KittyPopLatch::release();
    }

    #[test]
    fn mouse_enable_sequence_for_mux_capabilities_is_safe() {
        let mux_caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .in_wezterm_mux(true)
            .build();
        assert_eq!(
            mouse_enable_sequence_for_capabilities(mux_caps),
            MOUSE_ENABLE_MUX_SAFE
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE
                .windows(b"\x1b[?1005l".len())
                .any(|w| w == b"\x1b[?1005l"),
            "mux-safe enable should clear UTF-8 mouse encoding (1005)"
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE
                .windows(b"\x1b[?1015l".len())
                .any(|w| w == b"\x1b[?1015l"),
            "mux-safe enable should clear urxvt mouse encoding (1015)"
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE
                .windows(b"\x1b[?1006h".len())
                .any(|w| w == b"\x1b[?1006h"),
            "mux-safe enable should keep SGR mouse mode"
        );
        assert!(
            !MOUSE_ENABLE_MUX_SAFE
                .windows(b"\x1b[?1003h".len())
                .any(|w| w == b"\x1b[?1003h"),
            "mux-safe enable should avoid 1003 any-event mode"
        );
        let pos_1016l = MOUSE_ENABLE_MUX_SAFE
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mux-safe enable should clear 1016 before enabling SGR");
        let pos_1006h = MOUSE_ENABLE_MUX_SAFE
            .windows(b"\x1b[?1006h".len())
            .position(|w| w == b"\x1b[?1006h")
            .expect("mux-safe enable should include 1006 SGR mode");
        assert!(
            pos_1016l < pos_1006h,
            "mux-safe enable must emit 1016l before 1006h to preserve SGR mode"
        );
    }

    #[test]
    fn mouse_disable_sequence_for_mux_capabilities_clears_1016() {
        let mux_caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .in_wezterm_mux(true)
            .build();
        assert_eq!(
            mouse_disable_sequence_for_capabilities(mux_caps),
            MOUSE_DISABLE_MUX_SAFE
        );
        let pos_1016l = MOUSE_DISABLE_MUX_SAFE
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mux-safe disable should clear 1016");
        let pos_1006l = MOUSE_DISABLE_MUX_SAFE
            .windows(b"\x1b[?1006l".len())
            .position(|w| w == b"\x1b[?1006l")
            .expect("mux-safe disable should include 1006 reset");
        assert!(
            pos_1016l < pos_1006l,
            "mux-safe disable should clear 1016 before disabling 1006"
        );
    }

    #[test]
    fn feature_delta_uses_mux_safe_mouse_sequence() {
        let current = BackendFeatures::default();
        let new = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
        };
        let mux_caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .in_wezterm_mux(true)
            .build();
        let mut buf = Vec::new();
        TtyEventSource::write_feature_delta(&current, &new, mux_caps, &mut buf).unwrap();
        assert!(
            buf.windows(MOUSE_ENABLE_MUX_SAFE.len())
                .any(|w| w == MOUSE_ENABLE_MUX_SAFE),
            "feature delta should use mux-safe mouse enable sequence in mux contexts"
        );
        assert!(
            buf.windows(b"\x1b[?1005l".len())
                .any(|w| w == b"\x1b[?1005l"),
            "feature delta should clear UTF-8 mouse encoding (1005) in mux contexts"
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
        TtyEventSource::write_feature_delta(
            &current,
            &new,
            TerminalCapabilities::modern(),
            &mut buf,
        )
        .unwrap();
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
        TtyEventSource::write_feature_delta(
            &features,
            &features,
            TerminalCapabilities::modern(),
            &mut buf,
        )
        .unwrap();
        assert!(buf.is_empty(), "no output expected when features unchanged");
    }

    #[test]
    fn cleanup_sequence_contains_all_disable() {
        let _latch_guard = kitty_latch_test_lock();
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let mut buf = Vec::new();
        ftui_core::session_teardown::KittyPopLatch::release();
        write_cleanup_sequence(&features, true, &mut buf).unwrap();

        // Verify expected cleanup disables are present.
        assert!(
            !buf.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "default cleanup utility must not emit standalone sync_end"
        );
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
    fn cleanup_sequence_with_sync_end_opt_in() {
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
        };
        let mut buf = Vec::new();
        write_cleanup_sequence_with_sync_end(&features, true, &mut buf).unwrap();

        assert!(
            buf.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "opt-in cleanup helper should include sync_end"
        );
        let sync_pos = buf
            .windows(SYNC_END.len())
            .position(|w| w == SYNC_END)
            .expect("sync_end present");
        let cursor_pos = buf
            .windows(CURSOR_SHOW.len())
            .position(|w| w == CURSOR_SHOW)
            .expect("cursor_show present");
        assert!(
            sync_pos < cursor_pos,
            "sync_end should precede cursor_show in opt-in cleanup"
        );
    }

    #[test]
    fn cleanup_sequence_policy_can_skip_sync_end() {
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
        };
        let mut buf = Vec::new();
        write_cleanup_sequence_policy(&features, false, false, &mut buf).unwrap();

        assert!(
            !buf.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "sync_end must be omitted when policy disables synchronized output"
        );
        assert!(
            buf.windows(MOUSE_DISABLE.len()).any(|w| w == MOUSE_DISABLE),
            "other cleanup bytes must still be emitted"
        );
        assert!(buf.windows(CURSOR_SHOW.len()).any(|w| w == CURSOR_SHOW));
    }

    #[test]
    fn core_cleanup_and_tty_cleanup_produce_identical_bytes() {
        use ftui_core::session_teardown::KittyPopLatch;
        use ftui_core::terminal_capabilities::TerminalCapabilities;
        use ftui_core::terminal_session::best_effort_cleanup_to;

        let _latch_guard = kitty_latch_test_lock();

        for (mouse, paste, focus, kitty) in [
            (true, true, true, true),
            (true, false, true, false),
            (false, true, false, true),
            (false, false, false, false),
        ] {
            let mut caps = TerminalCapabilities::modern();
            caps.mouse_sgr = mouse;
            caps.bracketed_paste = paste;
            caps.focus_events = focus;
            caps.kitty_keyboard = kitty;

            let features = BackendFeatures {
                mouse_capture: mouse,
                bracketed_paste: paste,
                focus_events: focus,
                kitty_keyboard: kitty,
            };

            let mut core_buf = Vec::new();
            KittyPopLatch::release();
            best_effort_cleanup_to(&mut core_buf, &caps);

            // Each backend must be measured from the same starting state: the
            // latch is one-shot, so without this reset the core call above would
            // have claimed it and tty would correctly decline to pop again.
            let mut tty_buf = Vec::new();
            KittyPopLatch::release();
            write_cleanup_sequence_with_sync_end(&features, true, &mut tty_buf).unwrap();

            assert_eq!(
                core_buf, tty_buf,
                "core and tty cleanup must produce identical bytes for features={features:?}"
            );
        }
    }

    #[test]
    fn conservative_feature_union_is_over_disabling_superset() {
        let a = BackendFeatures {
            mouse_capture: false,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: true,
        };
        let b = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: true,
            kitty_keyboard: false,
        };

        let merged = conservative_feature_union(a, b);
        assert!(merged.mouse_capture);
        assert!(merged.bracketed_paste);
        assert!(merged.focus_events);
        assert!(merged.kitty_keyboard);
    }

    #[test]
    fn sanitize_feature_request_disables_unsupported_capabilities() {
        let requested = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let sanitized = sanitize_feature_request(requested, TerminalCapabilities::basic());
        assert_eq!(sanitized, BackendFeatures::default());
    }

    #[test]
    fn sanitize_feature_request_is_conservative_in_wezterm_mux() {
        let requested = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .bracketed_paste(true)
            .focus_events(true)
            .kitty_keyboard(true)
            .in_wezterm_mux(true)
            .build();
        let sanitized = sanitize_feature_request(requested, caps);

        assert!(
            sanitized.mouse_capture,
            "mouse capture should remain available"
        );
        assert!(
            sanitized.bracketed_paste,
            "bracketed paste should remain available"
        );
        assert!(
            !sanitized.focus_events,
            "focus events should be disabled in wezterm mux"
        );
        assert!(
            !sanitized.kitty_keyboard,
            "kitty keyboard should be disabled in mux sessions"
        );
    }

    #[test]
    fn sanitize_feature_request_disables_focus_in_tmux() {
        let requested = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .bracketed_paste(true)
            .focus_events(true)
            .kitty_keyboard(true)
            .in_tmux(true)
            .build();
        let sanitized = sanitize_feature_request(requested, caps);

        assert!(sanitized.mouse_capture);
        assert!(sanitized.bracketed_paste);
        assert!(!sanitized.focus_events);
        assert!(!sanitized.kitty_keyboard);
    }

    #[cfg(unix)]
    #[test]
    fn signal_intercept_guard_disabled_reports_inactive() {
        let mut guard = SignalInterceptGuard::new(false);
        assert!(
            !guard.disarm(),
            "disabled guard should report inactive ownership"
        );
    }

    #[cfg(unix)]
    #[test]
    fn signal_intercept_guard_disarm_transfers_ownership() {
        let _serial = signal_counter_lock();
        let before = LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst);
        let mut guard = SignalInterceptGuard::new(true);
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before + 1,
            "arming should take a slot"
        );
        assert!(
            guard.disarm(),
            "enabled guard should report transferred ownership on disarm"
        );
        drop(guard);
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before + 1,
            "a disarmed guard must not release the slot it handed on"
        );
        // Stand in for the owner the slot was handed to.
        LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_sub(1, Ordering::SeqCst);
    }

    /// Serialize the tests that read [`LIVE_SIGNAL_INTERCEPT_SESSIONS`].
    ///
    /// The counter is process-global. nextest gives each test its own process,
    /// but `cargo test` does not, and these assertions are on exact values -
    /// without this they would be flaky under `cargo test` alone, which is the
    /// worst kind of test to leave behind.
    #[cfg(unix)]
    fn signal_counter_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    #[cfg(all(unix, not(panic = "abort")))]
    #[test]
    fn a_recovered_panic_leaves_the_tty_session_alone() {
        // The runtime recovers panics in tasks, subscriptions and screens
        // inside `with_panic_cleanup_suppressed`. This hook ignored that and
        // ran the teardown, so the app went on rendering in cooked mode on
        // the main screen, and the real teardown later found the session
        // already marked inactive.
        let _serial = signal_counter_lock();
        install_abort_panic_hook();
        TTY_SESSION_ACTIVE.store(true, Ordering::SeqCst);

        let recovered = ftui_core::with_panic_cleanup_suppressed(|| {
            std::panic::catch_unwind(|| panic!("recovered by the runtime"))
        });
        let still_active = TTY_SESSION_ACTIVE.swap(false, Ordering::SeqCst);

        assert!(recovered.is_err());
        assert!(still_active, "a recovered panic ran the tty teardown");
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_backend_returns_its_signal_slot_after_teardown_already_ran() {
        let _serial = signal_counter_lock();

        // The decrement used to live at the bottom of
        //
        //     if self.raw_mode.is_some() {
        //         if !TTY_SESSION_ACTIVE.swap(false, ..) { return; }
        //         ...emit the teardown sequence...
        //         <decrement here>
        //     }
        //
        // so *either* condition skipped it. The one this test can reach
        // without a controlling tty is the outer one - a headless backend
        // holding a slot - because `TtyBackend::new` leaves `raw_mode` as
        // `None`. The one the fix is really for is the inner one: after a
        // panic or a termination signal, `best_effort_termination_cleanup`
        // has already emitted the bytes and swapped `TTY_SESSION_ACTIVE` to
        // false, so a live backend dropping afterwards took the early return.
        // Same nesting, same lost slot; set the flag here to model that state
        // even though the outer condition is what bites first.
        TTY_SESSION_ACTIVE.store(false, Ordering::SeqCst);

        let before = LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst);
        {
            let mut backend = TtyBackend::new(80, 24);
            LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_add(1, Ordering::SeqCst);
            backend.signal_interception_active = true;
            assert_eq!(
                LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
                before + 1
            );
        }
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before,
            "the slot must come back even when the teardown guard is spent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn releasing_signal_interception_twice_only_gives_back_one_slot() {
        let _serial = signal_counter_lock();
        let before = LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst);

        let mut backend = TtyBackend::new(80, 24);
        LIVE_SIGNAL_INTERCEPT_SESSIONS.fetch_add(1, Ordering::SeqCst);
        backend.signal_interception_active = true;

        backend.release_signal_interception();
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before
        );
        backend.release_signal_interception();
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before,
            "release must be idempotent - the Drop that follows calls it again"
        );
        drop(backend);
        assert_eq!(
            LIVE_SIGNAL_INTERCEPT_SESSIONS.load(Ordering::SeqCst),
            before,
            "and the drop must not take a slot that was already returned"
        );
    }

    #[test]
    fn apply_feature_state_enables_legacy_fallbacks_when_mouse_capture_on() {
        let mut src = TtyEventSource::new(80, 24);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.apply_feature_state(BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        });

        // With SGR support, keep numeric and raw X10 fallbacks enabled for
        // mux/terminal edge-cases that ignore SGR mode requests.
        let modern_events = src.parser.parse(b"\x1b[0;10;20M");
        assert!(
            modern_events.iter().any(|e| matches!(e, Event::Mouse(_))),
            "legacy numeric fallback should remain available with mouse capture on"
        );
        let modern_x10 = src.parser.parse(&[0x1B, b'[', b'M', 32, 42, 52]);
        assert!(
            modern_x10.iter().any(|e| matches!(e, Event::Mouse(_))),
            "raw X10 fallback should stay available with mouse capture on"
        );

        src.capabilities = TerminalCapabilities::basic();
        src.apply_feature_state(BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        });

        // Without SGR support, fallback remains enabled.
        let legacy_events = src.parser.parse(b"\x1b[0;10;20M");
        assert!(
            legacy_events.iter().any(|e| matches!(e, Event::Mouse(_))),
            "legacy mouse fallback should be enabled when SGR is unavailable"
        );
        let legacy_x10 = src.parser.parse(&[0x1B, b'[', b'M', 32, 42, 52]);
        assert!(
            legacy_x10.iter().any(|e| matches!(e, Event::Mouse(_))),
            "raw X10 decoding should be enabled when SGR is unavailable"
        );

        src.apply_feature_state(BackendFeatures::default());
        let disabled_x10 = src.parser.parse(&[0x1B, b'[', b'M', 32, 42, 52]);
        assert!(
            disabled_x10.iter().all(|e| !matches!(e, Event::Mouse(_))),
            "raw X10 fallback must be disabled when mouse capture is off"
        );
    }

    #[test]
    fn normalize_event_maps_pixel_space_mouse_to_cell_grid() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut src = TtyEventSource::new(100, 40);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.features = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        src.pixel_width = 1000;
        src.pixel_height = 800;

        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 500,
            y: 400,
            modifiers: Modifiers::NONE,
        });
        let normalized = src.normalize_event(event);

        let mouse = match normalized {
            Event::Mouse(mouse) => mouse,
            other => {
                panic!("expected mouse event, got {other:?}");
            }
        };
        assert!(mouse.x < src.width, "x should be mapped into cell bounds");
        assert!(mouse.y < src.height, "y should be mapped into cell bounds");
        assert!(
            mouse.x > 0 && mouse.y > 0,
            "pixel-space event should not collapse to origin"
        );
    }

    #[test]
    fn pixel_coordinates_map_to_the_cell_that_holds_them() {
        // 100 cells over 1000 pixels: cell c is pixels 10c..=10c+9. Pixel 10
        // came out as cell 0, and pixel 990 as cell 98.
        for (pixel, cell) in [(0, 0), (9, 0), (10, 1), (505, 50), (989, 98), (990, 99)] {
            assert_eq!(
                TtyEventSource::scale_mouse_coord(pixel, 100, 1000),
                cell,
                "pixel {pixel}"
            );
        }
        // Past the reported extent clamps to the last cell.
        assert_eq!(TtyEventSource::scale_mouse_coord(1500, 100, 1000), 99);
        assert_eq!(TtyEventSource::scale_mouse_coord(u16::MAX, 100, 1000), 99);
    }

    #[test]
    fn normalize_event_keeps_cell_space_mouse_unchanged() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut src = TtyEventSource::new(100, 40);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.features = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        src.pixel_width = 1000;
        src.pixel_height = 800;

        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 50,
            y: 10,
            modifiers: Modifiers::NONE,
        });
        let normalized = src.normalize_event(event.clone());
        assert_eq!(
            normalized, event,
            "cell-space coordinates must be preserved"
        );
    }

    #[test]
    fn normalize_event_sticky_pixel_mode_maps_subsequent_low_coordinates() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut src = TtyEventSource::new(100, 40);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.features = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        src.pixel_width = 1000;
        src.pixel_height = 800;

        let first = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 700,
            y: 500,
            modifiers: Modifiers::NONE,
        });
        let _ = src.normalize_event(first);
        assert!(
            src.mouse_coords_pixels,
            "large out-of-grid mouse event should arm sticky pixel normalization"
        );

        let second = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 100,
            y: 20,
            modifiers: Modifiers::NONE,
        });
        let normalized = src.normalize_event(second);
        let mouse = match normalized {
            Event::Mouse(mouse) => mouse,
            other => {
                panic!("expected mouse event, got {other:?}");
            }
        };
        assert!(mouse.x < src.width, "sticky mode should normalize x");
        assert!(mouse.y < src.height, "sticky mode should normalize y");
    }

    #[test]
    fn apply_feature_state_disabling_mouse_resets_pixel_detector() {
        let mut src = TtyEventSource::new(80, 24);
        src.mouse_coords_pixels = true;
        src.inferred_pixel_width = 1234;
        src.inferred_pixel_height = 777;
        src.apply_feature_state(BackendFeatures::default());
        assert!(
            !src.mouse_coords_pixels,
            "disabling mouse capture should clear sticky pixel-mode detector"
        );
        assert_eq!(src.inferred_pixel_width, 0);
        assert_eq!(src.inferred_pixel_height, 0);
    }

    #[test]
    fn normalize_event_infers_pixel_grid_when_winsize_pixels_missing() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut src = TtyEventSource::new(100, 40);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.features = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        // Simulate terminals that leak pixel coordinates but report 0x0 pixel winsize.
        src.pixel_width = 0;
        src.pixel_height = 0;

        let first = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 700,
            y: 500,
            modifiers: Modifiers::NONE,
        });
        let normalized_first = src.normalize_event(first);
        let first_mouse = match normalized_first {
            Event::Mouse(mouse) => mouse,
            other => {
                panic!("expected mouse event, got {other:?}");
            }
        };
        assert!(first_mouse.x > 0 && first_mouse.x < src.width.saturating_sub(1));
        assert!(first_mouse.y > 0 && first_mouse.y < src.height.saturating_sub(1));

        let second = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            x: 250,
            y: 200,
            modifiers: Modifiers::NONE,
        });
        let normalized = src.normalize_event(second);
        let mouse = match normalized {
            Event::Mouse(mouse) => mouse,
            other => {
                panic!("expected mouse event, got {other:?}");
            }
        };

        assert!(mouse.x < src.width);
        assert!(mouse.y < src.height);
        assert!(mouse.x > 0 && mouse.x < src.width.saturating_sub(1));
        assert!(mouse.y > 0 && mouse.y < src.height.saturating_sub(1));
    }

    #[test]
    fn normalize_event_near_edge_outside_grid_clamps_without_sticky_pixel_mode() {
        use ftui_core::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut src = TtyEventSource::new(100, 40);
        src.capabilities = TerminalCapabilities::builder().mouse_sgr(true).build();
        src.features = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        src.pixel_width = 1000;
        src.pixel_height = 800;

        let near_edge = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: 100,
            y: 40,
            modifiers: Modifiers::NONE,
        });
        let normalized = src.normalize_event(near_edge);
        let mouse = match normalized {
            Event::Mouse(mouse) => mouse,
            other => {
                panic!("expected mouse event, got {other:?}");
            }
        };
        assert_eq!(mouse.x, 99);
        assert_eq!(mouse.y, 39);
        assert!(
            !src.mouse_coords_pixels,
            "edge clamp must not arm sticky pixel normalization"
        );

        let follow_up = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            x: 50,
            y: 20,
            modifiers: Modifiers::NONE,
        });
        let normalized_follow_up = src.normalize_event(follow_up);
        assert_eq!(
            normalized_follow_up,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                x: 50,
                y: 20,
                modifiers: Modifiers::NONE,
            }),
            "normal cell-space events should remain unchanged after edge clamp"
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

        // Verify ordering: cursor_show before alt_screen_leave.
        let cursor_pos = buf
            .windows(CURSOR_SHOW.len())
            .position(|w| w == CURSOR_SHOW)
            .expect("cursor_show present");
        let alt_pos = buf
            .windows(ALT_SCREEN_LEAVE.len())
            .position(|w| w == ALT_SCREEN_LEAVE)
            .expect("alt_screen_leave present");

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
                std::panic::panic_any("intentional panic for testing raw mode cleanup");
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
                    TerminalCapabilities::modern(),
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
            let _latch_guard = super::kitty_latch_test_lock();
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
                ftui_core::session_teardown::KittyPopLatch::release();
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
                std::panic::panic_any("panic with features enabled");
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
                write_cleanup_sequence_with_sync_end(&features, true, &mut seq).unwrap();

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
