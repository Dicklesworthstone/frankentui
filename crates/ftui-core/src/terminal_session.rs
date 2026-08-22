#![forbid(unsafe_code)]

//! Terminal session lifecycle guard.
//!
//! This module provides RAII-based terminal lifecycle management that ensures
//! cleanup even on panic. It owns raw-mode entry/exit and tracks all terminal
//! state changes.
//!
//! # Lifecycle Guarantees
//!
//! 1. **All terminal state changes are tracked** - Each mode (raw, alt-screen,
//!    mouse, bracketed paste, focus events) has a corresponding flag.
//!
//! 2. **Drop restores previous state** - When the [`TerminalSession`] is
//!    dropped, all enabled modes are disabled in reverse order.
//!
//! 3. **Panic safety** - Because cleanup is in [`Drop`], it runs during panic
//!    unwinding (unless `panic = "abort"` is set).
//!
//! 4. **No leaked state on any exit path** - Whether by return, `?`, panic,
//!    or `process::exit()` (excluding abort), terminal state is restored.
//!
//! # Backend Decision (ADR-003)
//!
//! This module uses Crossterm as the terminal backend. Key requirements:
//! - Raw mode enter/exit must be reliable
//! - Cleanup must happen on normal exit AND panic
//! - Resize events must be delivered accurately
//!
//! See ADR-003 for the full backend decision rationale.
//!
//! # Escape Sequences Reference
//!
//! The following escape sequences are used (via Crossterm):
//!
//! | Feature | Enable | Disable |
//! |---------|--------|---------|
//! | Alternate screen | `CSI ? 1049 h` | `CSI ? 1049 l` |
//! | Mouse (SGR) | reset legacy encodings, then `CSI ? 1000;1002;1006 h` (+ split compatibility) | `CSI ? 1000;1002;1006 l` (+ split compatibility, plus legacy resets) |
//! | Bracketed paste | `CSI ? 2004 h` | `CSI ? 2004 l` |
//! | Focus events | `CSI ? 1004 h` | `CSI ? 1004 l` |
//! | Kitty keyboard | `CSI > 15 u` | `CSI < u` |
//! | Show cursor | `CSI ? 25 h` | `CSI ? 25 l` |
//! | Reset style | `CSI 0 m` | N/A |
//!
//! # Cleanup Order
//!
//! On drop, cleanup happens in reverse order of enabling:
//! 1. Reset scroll region and style (`CSI r`, `CSI 0 m`)
//! 2. Disable kitty keyboard (if enabled)
//! 3. Disable focus events (if enabled)
//! 4. Disable bracketed paste (if enabled)
//! 5. Disable mouse capture (if enabled)
//! 6. Show cursor (always)
//! 7. Leave alternate screen (if enabled)
//! 8. Exit raw mode (always)
//! 9. Flush stdout
//!
//! # Usage
//!
//! ```no_run
//! use ftui_core::terminal_session::{TerminalSession, SessionOptions};
//!
//! // Create a session with desired options
//! let session = TerminalSession::new(SessionOptions {
//!     alternate_screen: true,
//!     mouse_capture: true,
//!     ..Default::default()
//! })?;
//!
//! // Terminal is now in raw mode with alt screen and mouse
//! // ... do work ...
//!
//! // When `session` is dropped, terminal is restored
//! # Ok::<(), std::io::Error>(())
//! ```

use std::cell::Cell;
use std::env;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::event::Event;
use crate::terminal_capabilities::TerminalCapabilities;

// Import tracing macros (no-op when tracing feature is disabled).
#[cfg(feature = "tracing")]
use crate::logging::{info_span, warn};
#[cfg(not(feature = "tracing"))]
use crate::{info_span, warn};

// ─── Metrics counters ────────────────────────────────────────────────────────

static IO_READ_DURATION_SUM_US: AtomicU64 = AtomicU64::new(0);
static IO_READ_COUNT: AtomicU64 = AtomicU64::new(0);
static IO_WRITE_DURATION_SUM_US: AtomicU64 = AtomicU64::new(0);
static IO_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
static IO_FLUSH_DURATION_SUM_US: AtomicU64 = AtomicU64::new(0);
static IO_FLUSH_COUNT: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static PANIC_CLEANUP_SUPPRESS_DEPTH: Cell<u32> = const { Cell::new(0) };
}

#[cfg(unix)]
const SIGNAL_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const SIGNAL_SHUTDOWN_POLL: Duration = Duration::from_millis(10);

/// Set when deliberate teardown has begun ([`TerminalSession::cleanup`]).
///
/// A termination signal that arrives AFTER the main loop's last signal check
/// would otherwise find nobody left to acknowledge it: the signal thread
/// would sit out the full grace period and then force-exit mid-`Drop`,
/// skipping the remaining destructors (bd-kdn7n). Once this latch is set,
/// `wait_for_shutdown_ack` returns immediately — the terminal is already
/// being restored by the explicit path, so the signal is treated as
/// acknowledged.
#[cfg(unix)]
static EXPLICIT_TEARDOWN_STARTED: AtomicBool = AtomicBool::new(false);
/// Returns (sum_us, count) for read I/O operations.
pub fn terminal_io_read_stats() -> (u64, u64) {
    (
        IO_READ_DURATION_SUM_US.load(Ordering::Relaxed),
        IO_READ_COUNT.load(Ordering::Relaxed),
    )
}

/// Returns (sum_us, count) for write I/O operations.
pub fn terminal_io_write_stats() -> (u64, u64) {
    (
        IO_WRITE_DURATION_SUM_US.load(Ordering::Relaxed),
        IO_WRITE_COUNT.load(Ordering::Relaxed),
    )
}

/// Returns (sum_us, count) for flush I/O operations.
pub fn terminal_io_flush_stats() -> (u64, u64) {
    (
        IO_FLUSH_DURATION_SUM_US.load(Ordering::Relaxed),
        IO_FLUSH_COUNT.load(Ordering::Relaxed),
    )
}

#[cfg(unix)]
fn wait_for_shutdown_ack() -> bool {
    // Deliberate teardown already started: the signal is acknowledged by the
    // explicit restore path itself (bd-kdn7n race — late SIGTERM after the
    // main loop's final check must not force-exit mid-Drop).
    if EXPLICIT_TEARDOWN_STARTED.load(Ordering::SeqCst) {
        return true;
    }
    let deadline = std::time::Instant::now()
        .checked_add(SIGNAL_SHUTDOWN_GRACE)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        if crate::shutdown_signal::pending_termination_signal().is_none() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SIGNAL_SHUTDOWN_POLL);
    }
}

/// Run a closure while suppressing best-effort terminal cleanup in the panic hook.
///
/// Use this around intentional `catch_unwind` boundaries. Panic hooks still run,
/// but terminal cleanup is skipped for panics that are expected to be recovered.
///
/// In `panic = "abort"` builds, panics cannot be recovered by `catch_unwind`,
/// so suppression is disabled and this executes `f` directly.
pub fn with_panic_cleanup_suppressed<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    #[cfg(panic = "abort")]
    {
        return f();
    }

    #[cfg(not(panic = "abort"))]
    {
        struct SuppressGuard;
        impl Drop for SuppressGuard {
            fn drop(&mut self) {
                PANIC_CLEANUP_SUPPRESS_DEPTH.with(|depth| {
                    depth.set(depth.get().saturating_sub(1));
                });
            }
        }

        PANIC_CLEANUP_SUPPRESS_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_add(1));
        });
        let _guard = SuppressGuard;
        f()
    }
}

fn panic_cleanup_suppressed() -> bool {
    PANIC_CLEANUP_SUPPRESS_DEPTH.with(|depth| depth.get() > 0)
}

/// Convert `web_time::Duration` to `std::time::Duration`, clamping to avoid
/// overflow on the `as_micros() -> u64` conversion.
fn to_std_duration(d: web_time::Duration) -> Duration {
    Duration::from_micros(d.as_micros().min(u64::MAX as u128) as u64)
}

const SIZE_RETRY_DELAY: Duration = Duration::from_millis(10);

#[inline]
fn size_retry_delay(cx: &crate::cx::Cx) -> Option<Duration> {
    if cx.is_done() {
        return None;
    }

    match cx.remaining() {
        Some(remaining) if to_std_duration(remaining) <= SIZE_RETRY_DELAY => None,
        _ => Some(SIZE_RETRY_DELAY),
    }
}

/// Compute remaining microseconds for Cx, or `u64::MAX` if no deadline.
#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
fn cx_deadline_remaining_us(cx: &crate::cx::Cx) -> u64 {
    cx.remaining()
        .map(|r| r.as_micros().min(u64::MAX as u128) as u64)
        .unwrap_or(u64::MAX)
}

const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";
const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";
const RESET_STYLE: &[u8] = b"\x1b[0m";
const SYNC_END: &[u8] = b"\x1b[?2026l";
// Mouse mode hygiene:
// 1) Reset legacy and alternate encodings.
// 2) Enable canonical SGR mouse modes (1000 + 1002 + 1006).
// 3) Clear 1016 before enabling SGR to avoid terminals that treat 1016l
//    as a hard fallback to X10 when sent after 1006h.
// 4) Avoid DECSET 1003 (any-event mouse) because high-rate move streams can
//    destabilize mux pipelines.
// NOTE: Set SGR format (1006) before enabling mouse event modes for better
// compatibility with terminals that key off "last mode set" ordering.
const MOUSE_ENABLE_SEQ: &[u8] = b"\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l\x1b[?1006;1000;1002h\x1b[?1006h\x1b[?1000h\x1b[?1002h";
// Conservative mouse enable sequence for mux sessions and runtime toggles.
// Keep to split DECSET/DECRST forms (better passthrough behavior), but still
// reset alternate encodings so the inner terminal doesn't get "stuck" in a
// format our parser won't decode.
const MOUSE_ENABLE_MUX_SAFE_SEQ: &[u8] =
    b"\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l\x1b[?1006h\x1b[?1000h\x1b[?1002h";
const MOUSE_DISABLE_SEQ: &[u8] = b"\x1b[?1000;1002;1006l\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l";
// Conservative mouse disable sequence for mux/panic cleanup paths. Keeps
// parser surface minimal while still restoring canonical capture modes and
// clearing any leaked SGR-pixel mode.
const MOUSE_DISABLE_MUX_SAFE_SEQ: &[u8] =
    b"\x1b[?1016l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1001l\x1b[?1005l\x1b[?1015l";

static TERMINAL_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set once any best-effort (panic/signal/exit) teardown has emitted the
/// stack-based kitty-keyboard pop (`CSI < u`).
///
/// The pop consumes one entry from the terminal's kitty-protocol stack, so it
/// must happen at most once per process push: a second emission (Drop cleanup
/// after the panic hook, or a repeated best-effort call) would discard an
/// *enclosing* tty context's entry (bd-kdn7n). Non-stack resets are safe to
/// repeat and stay ungated.
static BEST_EFFORT_KITTY_POP_EMITTED: AtomicBool = AtomicBool::new(false);

/// Process-wide serialization for terminal byte emission.
///
/// The signal/panic teardown paths (`best_effort_cleanup`,
/// `TerminalSession::cleanup`) and the render path (ftui-runtime's
/// `TerminalWriter`) both write escape sequences to the same stdout. Without
/// mutual exclusion, a frame flush in progress interleaves with teardown
/// bytes (`ESC[r`, `1049l`, ...) and the frame remainder lands on the cooked
/// screen (bd-kdn7n item 2, the one-writer violation).
///
/// Reentrant on purpose: the panic hook runs ON the rendering thread while it
/// may already hold this lock mid-frame, so same-thread re-entry must not
/// deadlock; cross-thread callers (the signal thread) still block until the
/// in-flight emission batch completes. Emission batches are short (one
/// buffered frame or one teardown sequence), so a signal thread waiting on
/// this lock is bounded by a single flush.
static TERMINAL_OUTPUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// Per-thread nesting depth for [`TERMINAL_OUTPUT_LOCK`]. The outermost
    /// acquisition on a thread owns the mutex; nested acquisitions (teardown
    /// re-entering from the panic hook on the render thread) hold no OS lock.
    static TERMINAL_OUTPUT_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Guard produced by [`terminal_output_lock`]; releases on drop.
#[derive(Debug)]
pub struct TerminalOutputGuard {
    /// Held only by the OUTERMOST acquisition on its thread; re-entries
    /// keep `None`.
    _inner: Option<std::sync::MutexGuard<'static, ()>>,
}

/// Acquire the process-wide terminal output lock (see
/// [`TERMINAL_OUTPUT_LOCK`]).
///
/// Emission sites hold this guard for the duration of one write batch:
/// teardown paths for their full restore sequence, `TerminalWriter` methods
/// per frame / log / control write. Reentrant per thread; cross-thread
/// callers block until the in-flight batch completes.
#[doc(hidden)]
#[must_use]
pub fn terminal_output_lock() -> TerminalOutputGuard {
    let is_outermost = TERMINAL_OUTPUT_DEPTH.with(|depth| {
        let current = depth.get();
        depth.set(current + 1);
        current == 0
    });
    // A panic while holding (e.g. a panicking write) poisons the mutex; the
    // teardown paths must still be able to acquire it, so poison is ignored.
    let inner = is_outermost.then(|| {
        TERMINAL_OUTPUT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    });
    TerminalOutputGuard { _inner: inner }
}

impl Drop for TerminalOutputGuard {
    fn drop(&mut self) {
        // Release the OS lock before decrementing so a waiting thread can
        // proceed immediately once this thread's outermost batch ends.
        self._inner = None;
        TERMINAL_OUTPUT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

#[derive(Debug)]
struct SessionLock;

impl SessionLock {
    fn acquire() -> io::Result<Self> {
        if TERMINAL_SESSION_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::other("TerminalSession already active"));
        }
        Ok(Self)
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        TERMINAL_SESSION_ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
#[cfg(unix)]
use signal_hook::iterator::Signals;

/// Terminal session configuration options.
///
/// These options control which terminal modes are enabled when a session
/// starts. All options default to `false` for maximum portability.
///
/// # Example
///
/// ```
/// use ftui_core::terminal_session::SessionOptions;
///
/// // Full-featured TUI
/// let opts = SessionOptions {
///     alternate_screen: true,
///     mouse_capture: true,
///     bracketed_paste: true,
///     focus_events: true,
///     ..Default::default()
/// };
///
/// // Minimal inline mode
/// let inline_opts = SessionOptions::default();
/// ```
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// Enable alternate screen buffer (`CSI ? 1049 h`).
    ///
    /// When enabled, the terminal switches to a separate screen buffer,
    /// preserving the original scrollback. On exit, the original screen
    /// is restored.
    ///
    /// Use this for full-screen applications. For inline mode (preserving
    /// scrollback), leave this `false`.
    pub alternate_screen: bool,

    /// Enable mouse capture with SGR encoding, resetting legacy/alternate
    /// mouse encodings first and then applying `CSI ? 1000;1002;1006 h`
    /// (plus split-form compatibility).
    ///
    /// Enables:
    /// - Normal mouse tracking (1000)
    /// - Button event tracking (1002)
    /// - SGR extended coordinates (1006) - supports coordinates > 223
    pub mouse_capture: bool,

    /// Enable bracketed paste mode (`CSI ? 2004 h`).
    ///
    /// When enabled, pasted text is wrapped in escape sequences:
    /// - Start: `ESC [ 200 ~`
    /// - End: `ESC [ 201 ~`
    ///
    /// This allows distinguishing pasted text from typed text.
    pub bracketed_paste: bool,

    /// Enable focus change events (`CSI ? 1004 h`).
    ///
    /// When enabled, the terminal sends events when focus is gained or lost:
    /// - Focus in: `ESC [ I`
    /// - Focus out: `ESC [ O`
    pub focus_events: bool,

    /// Enable Kitty keyboard protocol (pushes flags with `CSI > 15 u`).
    ///
    /// Uses the kitty protocol to report repeat/release events and disambiguate
    /// keys. This is optional and only supported by select terminals.
    pub kitty_keyboard: bool,

    /// Install a signal handler to restore terminal state on SIGINT/SIGTERM/SIGHUP.
    ///
    /// Defaults to `true` to ensure the terminal is not left in a broken state
    /// when the application is terminated. Disable this if you are managing
    /// signals yourself or running in a context where signal handling is undesirable.
    pub intercept_signals: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            alternate_screen: false,
            mouse_capture: false,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
            intercept_signals: true,
        }
    }
}

#[inline]
fn sanitize_session_options(
    mut requested: SessionOptions,
    capabilities: &TerminalCapabilities,
) -> SessionOptions {
    let focus_events_supported = capabilities.focus_events && !capabilities.in_any_mux();
    let kitty_keyboard_supported = capabilities.kitty_keyboard && !capabilities.in_any_mux();

    requested.mouse_capture = requested.mouse_capture && capabilities.mouse_sgr;
    requested.bracketed_paste = requested.bracketed_paste && capabilities.bracketed_paste;
    requested.focus_events = requested.focus_events && focus_events_supported;
    requested.kitty_keyboard = requested.kitty_keyboard && kitty_keyboard_supported;
    requested
}

/// A terminal session that manages raw mode and cleanup.
///
/// This struct owns the terminal configuration and ensures cleanup on drop.
/// It tracks all enabled modes and disables them in reverse order when dropped.
///
/// # Contract
///
/// - **Exclusive ownership**: Only one `TerminalSession` should exist at a time.
///   Creating multiple sessions will cause undefined terminal behavior.
///
/// - **Raw mode entry**: Creating a session automatically enters raw mode.
///   This disables line buffering and echo.
///
/// - **Cleanup guarantee**: When dropped (normally or via panic), all enabled
///   modes are disabled and the terminal is restored to its previous state.
///
/// # State Tracking
///
/// Each optional mode has a corresponding `_enabled` flag. These flags are
/// set when a mode is successfully enabled and cleared during cleanup.
/// This ensures we only disable modes that were actually enabled.
///
/// # Example
///
/// ```no_run
/// use ftui_core::terminal_session::{TerminalSession, SessionOptions};
///
/// fn run_app() -> std::io::Result<()> {
///     let session = TerminalSession::new(SessionOptions {
///         alternate_screen: true,
///         mouse_capture: true,
///         ..Default::default()
///     })?;
///
///     // Application loop
///     loop {
///         if session.poll_event(std::time::Duration::from_millis(100))? {
///             if let Some(event) = session.read_event()? {
///                 // Handle event...
///             }
///         }
///     }
///     // Session cleaned up when dropped
/// }
/// ```
#[derive(Debug)]
pub struct TerminalSession {
    /// Process-wide exclusivity guard for the one-session-at-a-time contract.
    ///
    /// Only sessions created via `TerminalSession::new` acquire this lock.
    /// `new_for_tests` intentionally skips it to allow parallel headless tests.
    session_lock: Option<SessionLock>,
    /// Whether this session must avoid mutating real terminal state.
    ///
    /// Test-helper sessions use this to skip global panic-hook installation and
    /// teardown writes on drop.
    headless: bool,
    options: SessionOptions,
    /// Track what was enabled so we can disable on drop.
    alternate_screen_enabled: bool,
    mouse_enabled: bool,
    bracketed_paste_enabled: bool,
    focus_events_enabled: bool,
    kitty_keyboard_enabled: bool,
    #[cfg(unix)]
    signal_guard: Option<SignalGuard>,
}

impl TerminalSession {
    #[inline]
    fn mouse_enable_sequence_for_caps(caps: &TerminalCapabilities) -> &'static [u8] {
        if caps.in_any_mux() {
            MOUSE_ENABLE_MUX_SAFE_SEQ
        } else {
            MOUSE_ENABLE_SEQ
        }
    }

    #[inline]
    fn mouse_disable_sequence_for_caps(caps: &TerminalCapabilities) -> &'static [u8] {
        if caps.in_any_mux() {
            MOUSE_DISABLE_MUX_SAFE_SEQ
        } else {
            MOUSE_DISABLE_SEQ
        }
    }

    /// Enter raw mode and optionally enable additional features.
    ///
    /// # Errors
    ///
    /// Returns an error if raw mode cannot be enabled.
    pub fn new(options: SessionOptions) -> io::Result<Self> {
        install_panic_hook();
        let capabilities = TerminalCapabilities::with_overrides();
        let options = sanitize_session_options(options, &capabilities);

        let session_lock = SessionLock::acquire()?;

        // Create signal guard before raw mode so that a failure here
        // does not leave the terminal in raw mode (the struct would never
        // be fully constructed, so Drop would not run).
        #[cfg(unix)]
        let signal_guard = if options.intercept_signals {
            Some(SignalGuard::new()?)
        } else {
            None
        };

        // Enter raw mode
        crossterm::terminal::enable_raw_mode()?;
        #[cfg(feature = "tracing")]
        tracing::info!("terminal raw mode enabled");

        let mut session = Self {
            session_lock: Some(session_lock),
            headless: false,
            options: options.clone(),
            alternate_screen_enabled: false,
            mouse_enabled: false,
            bracketed_paste_enabled: false,
            focus_events_enabled: false,
            kitty_keyboard_enabled: false,
            #[cfg(unix)]
            signal_guard,
        };

        // Enable optional features
        let mut stdout = io::stdout();

        if options.alternate_screen {
            // Mark enabled before writing in case of partial write failures;
            // drop cleanup must unwind any state that may already be active.
            session.alternate_screen_enabled = true;
            // Enter alternate screen and explicitly clear it.
            // Some terminals (including WezTerm) may show stale content in the
            // alt-screen buffer without an explicit clear. We also position the
            // cursor at the top-left to ensure a known initial state.
            crossterm::execute!(
                stdout,
                crossterm::terminal::EnterAlternateScreen,
                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            )?;
            #[cfg(feature = "tracing")]
            tracing::info!("alternate screen enabled (with clear)");
        }

        if options.mouse_capture {
            session.mouse_enabled = true;
            let enable_seq = Self::mouse_enable_sequence_for_caps(&capabilities);
            stdout.write_all(enable_seq)?;
            stdout.flush()?;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture enabled");
        }

        if options.bracketed_paste {
            session.bracketed_paste_enabled = true;
            crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste)?;
            #[cfg(feature = "tracing")]
            tracing::info!("bracketed paste enabled");
        }

        if options.focus_events {
            session.focus_events_enabled = true;
            crossterm::execute!(stdout, crossterm::event::EnableFocusChange)?;
            #[cfg(feature = "tracing")]
            tracing::info!("focus events enabled");
        }

        if options.kitty_keyboard {
            session.kitty_keyboard_enabled = true;
            Self::enable_kitty_keyboard(&mut stdout)?;
            #[cfg(feature = "tracing")]
            tracing::info!("kitty keyboard enabled");
        }

        Ok(session)
    }

    /// Create a session for tests without entering raw mode or installing
    /// global terminal cleanup hooks.
    ///
    /// This skips raw mode and feature toggles, allowing headless tests
    /// to construct `TerminalSession` safely.
    #[cfg(feature = "test-helpers")]
    pub fn new_for_tests(options: SessionOptions) -> io::Result<Self> {
        #[cfg(unix)]
        let signal_guard = None;

        Ok(Self {
            session_lock: None,
            headless: true,
            options,
            alternate_screen_enabled: false,
            mouse_enabled: false,
            bracketed_paste_enabled: false,
            focus_events_enabled: false,
            kitty_keyboard_enabled: false,
            #[cfg(unix)]
            signal_guard,
        })
    }

    /// Create a minimal session (raw mode only).
    pub fn minimal() -> io::Result<Self> {
        Self::new(SessionOptions::default())
    }

    /// Get the current terminal size (columns, rows).
    pub fn size(&self) -> io::Result<(u16, u16)> {
        let (w, h) = crossterm::terminal::size()?;
        if w > 1 && h > 1 {
            return Ok((w, h));
        }

        // Some terminals briefly report 1x1 on startup; fall back to env vars when available.
        if let Some((env_w, env_h)) = size_from_env() {
            return Ok((env_w, env_h));
        }

        // Re-probe once after a short delay to catch terminals that report size late.
        std::thread::sleep(SIZE_RETRY_DELAY);
        let (w2, h2) = crossterm::terminal::size()?;
        if w2 > 1 && h2 > 1 {
            return Ok((w2, h2));
        }

        // Ensure minimum viable size to prevent downstream panics in buffer allocation
        // and layout calculations. 2x2 is the absolute minimum for a functional TUI.
        let final_w = w2.max(2);
        let final_h = h2.max(2);
        Ok((final_w, final_h))
    }

    /// Poll for an event with a timeout.
    ///
    /// Returns `Ok(true)` if an event is available, `Ok(false)` if timeout.
    pub fn poll_event(&self, timeout: std::time::Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    /// Poll for an event, respecting the `Cx` deadline and cancellation.
    ///
    /// The effective timeout is `min(timeout, cx.remaining())`. Returns
    /// `Ok(false)` immediately if the context is cancelled or expired.
    ///
    /// Emits a `terminal.io` tracing span with `op_type="poll"`.
    pub fn poll_event_cx(
        &self,
        timeout: std::time::Duration,
        cx: &crate::cx::Cx,
    ) -> io::Result<bool> {
        let _span = info_span!(
            "terminal.io",
            op_type = "poll",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(false);
        }
        let effective = match cx.remaining() {
            Some(rem) => timeout.min(to_std_duration(rem)),
            None => timeout,
        };
        let start = web_time::Instant::now();
        let result = crossterm::event::poll(effective);
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_READ_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_READ_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io poll completed after Cx deadline/cancellation");
        }
        result
    }

    /// Read the next event (blocking until available).
    ///
    /// Returns `Ok(None)` if the event cannot be represented by the
    /// ftui canonical event types (e.g. unsupported key codes).
    pub fn read_event(&self) -> io::Result<Option<Event>> {
        let event = crossterm::event::read()?;
        Ok(Event::from_crossterm(event))
    }

    /// Read the next event, respecting the `Cx` deadline and cancellation.
    ///
    /// Polls with the context's remaining deadline, then reads if available.
    /// Returns `Ok(None)` if the context is cancelled, expired, or the poll
    /// times out before an event arrives.
    ///
    /// Emits a `terminal.io` tracing span with `op_type="read"`.
    pub fn read_event_cx(&self, cx: &crate::cx::Cx) -> io::Result<Option<Event>> {
        let _span = info_span!(
            "terminal.io",
            op_type = "read",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(None);
        }
        let remaining = cx.remaining().unwrap_or(web_time::Duration::from_secs(60));
        let timeout = to_std_duration(remaining);
        let start = web_time::Instant::now();
        let result = if crossterm::event::poll(timeout)? {
            let event = crossterm::event::read()?;
            Ok(Event::from_crossterm(event))
        } else {
            Ok(None)
        };
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_READ_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_READ_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io read completed after Cx deadline/cancellation");
        }
        result
    }

    /// Show the cursor.
    pub fn show_cursor(&self) -> io::Result<()> {
        if self.headless {
            return Ok(());
        }
        crossterm::execute!(io::stdout(), crossterm::cursor::Show)
    }

    /// Show the cursor, respecting the `Cx` deadline and cancellation.
    ///
    /// Returns `Ok(())` without writing if the context is already done.
    pub fn show_cursor_cx(&self, cx: &crate::cx::Cx) -> io::Result<()> {
        let _span = info_span!(
            "terminal.io",
            op_type = "write",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(());
        }
        let start = web_time::Instant::now();
        if self.headless {
            return Ok(());
        }
        let result = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_WRITE_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io show_cursor completed after Cx deadline/cancellation");
        }
        result
    }

    /// Hide the cursor.
    pub fn hide_cursor(&self) -> io::Result<()> {
        if self.headless {
            return Ok(());
        }
        crossterm::execute!(io::stdout(), crossterm::cursor::Hide)
    }

    /// Hide the cursor, respecting the `Cx` deadline and cancellation.
    ///
    /// Returns `Ok(())` without writing if the context is already done.
    pub fn hide_cursor_cx(&self, cx: &crate::cx::Cx) -> io::Result<()> {
        let _span = info_span!(
            "terminal.io",
            op_type = "write",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(());
        }
        let start = web_time::Instant::now();
        if self.headless {
            return Ok(());
        }
        let result = crossterm::execute!(io::stdout(), crossterm::cursor::Hide);
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_WRITE_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io hide_cursor completed after Cx deadline/cancellation");
        }
        result
    }

    /// Return whether mouse capture is currently enabled for this session.
    ///
    /// Mouse capture enables terminal mouse reporting (SGR mode) so the runtime
    /// can receive click/scroll/drag events.
    #[must_use]
    pub fn mouse_capture_enabled(&self) -> bool {
        self.mouse_enabled
    }

    /// Enable or disable terminal mouse capture (SGR mouse reporting).
    ///
    /// This is idempotent: enabling when already enabled (or disabling when
    /// already disabled) is a no-op.
    ///
    /// Note: In many terminals, enabling mouse capture steals the scroll wheel
    /// from the terminal's native scrollback. In inline mode, prefer leaving
    /// this off unless the user explicitly opts in.
    pub fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        let caps = TerminalCapabilities::with_overrides();
        let mouse_supported = caps.mouse_sgr;
        let enabled = enabled && mouse_supported;

        // Honor the headless contract: track state, never write to the
        // real stdout (tests that toggle mouse capture must not flip SGR
        // mouse reporting in the developer's live terminal).
        if self.headless {
            self.mouse_enabled = enabled;
            self.options.mouse_capture = enabled;
            return Ok(());
        }

        if enabled == self.mouse_enabled {
            self.options.mouse_capture = enabled;
            return Ok(());
        }

        let mut stdout = io::stdout();
        let write_result = if enabled {
            let enable_seq = Self::mouse_enable_sequence_for_caps(&caps);
            stdout.write_all(enable_seq).and_then(|_| stdout.flush())
        } else {
            let disable_seq = Self::mouse_disable_sequence_for_caps(&caps);
            stdout.write_all(disable_seq).and_then(|_| stdout.flush())
        };

        if let Err(err) = write_result {
            // A failed write may have partially toggled mouse reporting.
            // Keep a conservative enabled state so cleanup always emits disable.
            self.mouse_enabled = self.mouse_enabled || enabled;
            self.options.mouse_capture = self.mouse_enabled;
            return Err(err);
        }

        if enabled {
            self.mouse_enabled = true;
            self.options.mouse_capture = true;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture enabled (runtime toggle)");
        } else {
            self.mouse_enabled = false;
            self.options.mouse_capture = false;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture disabled (runtime toggle)");
        }

        Ok(())
    }

    /// Enable or disable terminal mouse capture, respecting the `Cx` deadline.
    ///
    /// Returns `Ok(())` without writing if the context is already done.
    pub fn set_mouse_capture_cx(&mut self, enabled: bool, cx: &crate::cx::Cx) -> io::Result<()> {
        let _span = info_span!(
            "terminal.io",
            op_type = "write",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(());
        }
        let caps = TerminalCapabilities::with_overrides();
        let mouse_supported = caps.mouse_sgr;
        let enabled = enabled && mouse_supported;

        // Headless contract: track state, never write to real stdout.
        if self.headless {
            self.mouse_enabled = enabled;
            self.options.mouse_capture = enabled;
            return Ok(());
        }

        if enabled == self.mouse_enabled {
            self.options.mouse_capture = enabled;
            return Ok(());
        }
        let start = web_time::Instant::now();
        let mut stdout = io::stdout();
        let result = if enabled {
            let enable_seq = Self::mouse_enable_sequence_for_caps(&caps);
            let r = stdout.write_all(enable_seq).and_then(|_| stdout.flush());
            if r.is_ok() {
                self.mouse_enabled = true;
                self.options.mouse_capture = true;
            } else {
                // Conservative fallback: if the write partially succeeded, keep
                // mouse marked enabled so teardown still emits disable.
                self.mouse_enabled = self.mouse_enabled || enabled;
                self.options.mouse_capture = self.mouse_enabled;
            }
            r
        } else {
            let disable_seq = Self::mouse_disable_sequence_for_caps(&caps);
            let r = stdout.write_all(disable_seq).and_then(|_| stdout.flush());
            if r.is_ok() {
                self.mouse_enabled = false;
                self.options.mouse_capture = false;
            } else {
                // Conservative fallback for partial disable writes.
                self.mouse_enabled = true;
                self.options.mouse_capture = true;
            }
            r
        };
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_WRITE_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io set_mouse_capture completed after Cx deadline/cancellation");
        }
        result
    }

    /// Query terminal size, respecting the `Cx` deadline and cancellation.
    ///
    /// Skips the retry-with-delay fallback if the context is done.
    pub fn size_cx(&self, cx: &crate::cx::Cx) -> io::Result<(u16, u16)> {
        let _span = info_span!(
            "terminal.io",
            op_type = "read",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            // Return env fallback or minimum viable size.
            if let Some(env_size) = size_from_env() {
                return Ok(env_size);
            }
            return Ok((2, 2));
        }
        let start = web_time::Instant::now();
        let (w, h) = crossterm::terminal::size()?;
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_READ_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_READ_COUNT.fetch_add(1, Ordering::Relaxed);
        if w > 1 && h > 1 {
            return Ok((w, h));
        }
        if let Some((env_w, env_h)) = size_from_env() {
            return Ok((env_w, env_h));
        }
        // Skip the startup retry when the context no longer has enough budget
        // to pay for the delay and follow-up probe.
        let Some(retry_delay) = size_retry_delay(cx) else {
            return Ok((w.max(2), h.max(2)));
        };
        std::thread::sleep(retry_delay);
        let (w2, h2) = crossterm::terminal::size()?;
        if w2 > 1 && h2 > 1 {
            return Ok((w2, h2));
        }
        Ok((w2.max(2), h2.max(2)))
    }

    /// Flush stdout, respecting the `Cx` deadline and cancellation.
    ///
    /// Returns `Ok(())` without flushing if the context is already done.
    pub fn flush_cx(&self, cx: &crate::cx::Cx) -> io::Result<()> {
        let _span = info_span!(
            "terminal.io",
            op_type = "flush",
            cx_deadline_remaining_us = cx_deadline_remaining_us(cx),
            cx_cancelled = cx.is_cancelled()
        );
        let _guard = _span.enter();
        if cx.is_done() {
            return Ok(());
        }
        let start = web_time::Instant::now();
        let result = io::stdout().flush();
        let elapsed_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        IO_FLUSH_DURATION_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        IO_FLUSH_COUNT.fetch_add(1, Ordering::Relaxed);
        if cx.is_done() {
            warn!("terminal.io flush completed after Cx deadline/cancellation");
        }
        result
    }

    /// Get the session options.
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// Cleanup helper (shared between drop and explicit cleanup).
    fn cleanup(&mut self) {
        // Latch BEFORE dropping the guard: a termination signal racing this
        // teardown sees the explicit restore in progress and acknowledges
        // immediately instead of force-exiting mid-Drop (bd-kdn7n).
        #[cfg(unix)]
        EXPLICIT_TEARDOWN_STARTED.store(true, Ordering::SeqCst);

        // Serialize against the render path (bd-kdn7n item 2).
        let _output_guard = terminal_output_lock();

        #[cfg(unix)]
        let _ = self.signal_guard.take();

        if self.headless {
            self.alternate_screen_enabled = false;
            self.mouse_enabled = false;
            self.bracketed_paste_enabled = false;
            self.focus_events_enabled = false;
            self.kitty_keyboard_enabled = false;
            let _ = self.session_lock.take();
            return;
        }

        let mut stdout = io::stdout();
        let caps = TerminalCapabilities::with_overrides();

        // Reset scroll region (critical for inline mode recovery). DECSTBM —
        // including the parameterless reset — homes the cursor to (1,1) on
        // real terminals, which would discard the writer's deliberate final
        // cursor placement in inline mode (shell prompt at top-left over
        // scrollback). Bracket with DECSC/DECRC to preserve the cursor.
        let _ = stdout.write_all(b"\x1b7");
        let _ = stdout.write_all(RESET_SCROLL_REGION);
        let _ = stdout.write_all(b"\x1b8");
        // Reset style so shell prompt does not inherit UI SGR state.
        let _ = stdout.write_all(RESET_STYLE);
        // Ensure synchronized output is disabled (prevent frozen terminal on panic)
        let _ = stdout.write_all(SYNC_END);

        // Disable features in reverse order of enabling
        if self.kitty_keyboard_enabled {
            // A panic/signal best-effort teardown may already have emitted
            // the stack-based pop; repeating it would consume an enclosing
            // tty context's kitty entry (bd-kdn7n).
            if !BEST_EFFORT_KITTY_POP_EMITTED.load(Ordering::SeqCst) {
                let _ = Self::disable_kitty_keyboard(&mut stdout);
                #[cfg(feature = "tracing")]
                tracing::info!("kitty keyboard disabled");
            }
            self.kitty_keyboard_enabled = false;
        }
        if self.focus_events_enabled {
            let _ = crossterm::execute!(stdout, crossterm::event::DisableFocusChange);
            self.focus_events_enabled = false;
            #[cfg(feature = "tracing")]
            tracing::info!("focus events disabled");
        }

        if self.bracketed_paste_enabled {
            let _ = crossterm::execute!(stdout, crossterm::event::DisableBracketedPaste);
            self.bracketed_paste_enabled = false;
            #[cfg(feature = "tracing")]
            tracing::info!("bracketed paste disabled");
        }

        if self.mouse_enabled {
            let _ = stdout.write_all(Self::mouse_disable_sequence_for_caps(&caps));
            self.mouse_enabled = false;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture disabled");
        }

        // Always show cursor before leaving
        let _ = crossterm::execute!(stdout, crossterm::cursor::Show);

        if self.alternate_screen_enabled {
            let _ = crossterm::execute!(stdout, crossterm::terminal::LeaveAlternateScreen);
            self.alternate_screen_enabled = false;
            #[cfg(feature = "tracing")]
            tracing::info!("alternate screen disabled");
        }

        // Exit raw mode last
        let _ = crossterm::terminal::disable_raw_mode();
        #[cfg(feature = "tracing")]
        tracing::info!("terminal raw mode disabled");

        // Flush to ensure cleanup bytes are sent
        let _ = stdout.flush();

        // Release process-wide exclusivity only after terminal state is restored.
        let _ = self.session_lock.take();
    }

    fn enable_kitty_keyboard(writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(KITTY_KEYBOARD_ENABLE)?;
        writer.flush()
    }

    fn disable_kitty_keyboard(writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(KITTY_KEYBOARD_DISABLE)?;
        writer.flush()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn size_from_env() -> Option<(u16, u16)> {
    let cols = env::var("COLUMNS").ok()?.parse::<u16>().ok()?;
    let rows = env::var("LINES").ok()?.parse::<u16>().ok()?;
    if cols > 1 && rows > 1 {
        Some((cols, rows))
    } else {
        None
    }
}

fn install_panic_hook() {
    static HOOK: OnceLock<()> = OnceLock::new();
    HOOK.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Only touch the terminal while a session is actually live:
            // after the last session dropped cleanly, a later panic (e.g.
            // while emitting machine-readable output to a redirected
            // stdout) must not spray teardown escapes into the stream.
            if !panic_cleanup_suppressed() && TERMINAL_SESSION_ACTIVE.load(Ordering::SeqCst) {
                best_effort_cleanup();
            }
            previous(info);
        }));
    });
}

/// Best-effort cleanup for termination paths that skip `Drop`.
///
/// Call this before `std::process::exit` to restore terminal state when
/// unwinding won't run destructors.
pub fn best_effort_cleanup_for_exit() {
    best_effort_cleanup();
}

fn best_effort_cleanup() {
    let mut stdout = io::stdout();
    let caps = TerminalCapabilities::with_overrides();
    best_effort_cleanup_to(&mut stdout, &caps);
}

/// Emission body of [`best_effort_cleanup`], parameterized over the output
/// for testability. Consumes the process-global stack-pop budget: only the
/// first invocation emits the kitty-keyboard pop.
fn best_effort_cleanup_to(stdout: &mut impl Write, caps: &TerminalCapabilities) {
    // Serialize against the render path so teardown bytes cannot interleave
    // with an in-flight frame flush (bd-kdn7n item 2). Reentrant: the panic
    // hook may fire while the panicking render thread already holds the lock.
    let _output_guard = terminal_output_lock();
    // DECSC/DECRC bracket: DECSTBM reset homes the cursor on real
    // terminals; preserve whatever position the writer left it at.
    let _ = stdout.write_all(b"\x1b7");
    let _ = stdout.write_all(RESET_SCROLL_REGION);
    let _ = stdout.write_all(b"\x1b8");
    let _ = stdout.write_all(RESET_STYLE);
    let _ = stdout.write_all(SYNC_END);

    // Keep panic/signal cleanup conservative: only emit mux-sensitive mode
    // disables when policy says they could have been enabled. The kitty pop
    // is stack-based, so only the FIRST best-effort invocation emits it —
    // later invocations (hook → Drop → for_exit) must not pop an enclosing
    // tty context's entry (bd-kdn7n).
    let emit_stack_pops = !BEST_EFFORT_KITTY_POP_EMITTED.swap(true, Ordering::SeqCst);
    if emit_stack_pops && caps.kitty_keyboard && !caps.in_any_mux() {
        let _ = TerminalSession::disable_kitty_keyboard(stdout);
    }
    if caps.focus_events && !caps.in_any_mux() {
        let _ = crossterm::execute!(stdout, crossterm::event::DisableFocusChange);
    }
    let _ = crossterm::execute!(stdout, crossterm::event::DisableBracketedPaste);
    let _ = stdout.write_all(TerminalSession::mouse_disable_sequence_for_caps(caps));
    let _ = crossterm::execute!(stdout, crossterm::cursor::Show);
    let _ = crossterm::execute!(stdout, crossterm::terminal::LeaveAlternateScreen);
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = stdout.flush();
}

#[cfg(unix)]
#[derive(Debug)]
struct SignalGuard {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl SignalGuard {
    fn new() -> io::Result<Self> {
        let mut signals =
            Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGWINCH]).map_err(io::Error::other)?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            for signal in signals.forever() {
                match signal {
                    SIGWINCH => {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("SIGWINCH received");
                    }
                    SIGINT | SIGTERM | SIGHUP | SIGQUIT => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("termination signal received, cleaning up");
                        crate::shutdown_signal::record_pending_termination_signal(signal);
                        best_effort_cleanup();
                        if !wait_for_shutdown_ack() {
                            std::process::exit(128 + signal);
                        }
                    }
                    _ => {}
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
impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Spike validation notes (for ADR-003).
///
/// ## Crossterm Evaluation Results
///
/// ### Functionality (all verified)
/// - ✅ raw mode: `enable_raw_mode()` / `disable_raw_mode()`
/// - ✅ alternate screen: `EnterAlternateScreen` / `LeaveAlternateScreen`
/// - ✅ cursor show/hide: `Show` / `Hide`
/// - ✅ mouse mode (SGR): `EnableMouseCapture` / `DisableMouseCapture`
/// - ✅ bracketed paste: `EnableBracketedPaste` / `DisableBracketedPaste`
/// - ✅ focus events: `EnableFocusChange` / `DisableFocusChange`
/// - ✅ resize events: `Event::Resize(cols, rows)`
///
/// ### Robustness
/// - ✅ bounded-time reads via `poll()` with timeout
/// - ✅ handles partial sequences (internal buffer management)
/// - ⚠️ adversarial input: not fuzz-tested in this spike
///
/// ### Cleanup Discipline
/// - ✅ Drop impl guarantees cleanup on normal exit
/// - ✅ Drop impl guarantees cleanup on panic (via unwinding)
/// - ✅ cursor shown before exit
/// - ✅ raw mode disabled last
///
/// ### Platform Coverage
/// - ✅ Linux: fully supported
/// - ✅ macOS: fully supported
/// - ⚠️ Windows: supported with some feature limitations (see ADR-004)
///
/// ## Decision
/// **Crossterm is approved as the v1 terminal backend.**
///
/// Rationale: It provides all required functionality, handles cleanup via
/// standard Rust drop semantics, and has broad platform support.
///
/// Limitations documented in ADR-004 (Windows scope).
#[doc(hidden)]
pub const _SPIKE_NOTES: () = ();

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use portable_pty::{CommandBuilder, PtySize};
    #[cfg(unix)]
    use std::io::{self, Read, Write};
    #[cfg(unix)]
    use std::sync::mpsc;
    #[cfg(unix)]
    use std::thread;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[test]
    fn session_options_default_is_minimal() {
        let opts = SessionOptions::default();
        assert!(!opts.alternate_screen);
        assert!(!opts.mouse_capture);
        assert!(!opts.bracketed_paste);
        assert!(!opts.focus_events);
        assert!(!opts.kitty_keyboard);
    }

    #[test]
    fn session_options_clone() {
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: true,
            kitty_keyboard: false,
            intercept_signals: true,
        };
        let cloned = opts.clone();
        assert_eq!(cloned.alternate_screen, opts.alternate_screen);
        assert_eq!(cloned.mouse_capture, opts.mouse_capture);
        assert_eq!(cloned.bracketed_paste, opts.bracketed_paste);
        assert_eq!(cloned.focus_events, opts.focus_events);
        assert_eq!(cloned.kitty_keyboard, opts.kitty_keyboard);
    }

    #[test]
    fn session_options_debug() {
        let opts = SessionOptions::default();
        let debug = format!("{:?}", opts);
        assert!(debug.contains("SessionOptions"));
        assert!(debug.contains("alternate_screen"));
    }

    #[test]
    fn kitty_keyboard_escape_sequences() {
        // Verify the escape sequences are correct
        assert_eq!(KITTY_KEYBOARD_ENABLE, b"\x1b[>15u");
        assert_eq!(KITTY_KEYBOARD_DISABLE, b"\x1b[<u");
    }
    /// CONTRACT (bd-kdn7n): the stack-based kitty-keyboard pop is emitted by
    /// exactly ONE best-effort teardown invocation per process. Later
    /// invocations (panic hook → Drop cleanup → best_effort_cleanup_for_exit)
    /// must not pop again, or an enclosing tty context's kitty entry is lost.
    ///
    /// Note: consumes the process-global pop budget; run-order independent
    /// because no other test asserts on emitted `CSI < u` bytes.
    #[test]
    fn best_effort_cleanup_emits_kitty_pop_once() {
        let caps = TerminalCapabilities::modern();
        assert!(caps.kitty_keyboard);

        let mut first = Vec::new();
        best_effort_cleanup_to(&mut first, &caps);
        assert!(
            first
                .windows(KITTY_KEYBOARD_DISABLE.len())
                .any(|w| w == KITTY_KEYBOARD_DISABLE),
            "first best-effort teardown must emit the kitty pop"
        );

        let mut second = Vec::new();
        best_effort_cleanup_to(&mut second, &caps);
        assert!(
            !second
                .windows(KITTY_KEYBOARD_DISABLE.len())
                .any(|w| w == KITTY_KEYBOARD_DISABLE),
            "repeat best-effort teardown must NOT emit a second stack pop"
        );
    }

    /// CONTRACT (bd-kdn7n): once deliberate teardown has begun, a late
    /// termination signal is treated as acknowledged — `wait_for_shutdown_ack`
    /// returns immediately instead of burning the grace period and letting
    /// the signal thread force-exit mid-`Drop`.
    #[cfg(unix)]
    #[test]
    fn wait_for_shutdown_ack_immediate_after_explicit_teardown() {
        crate::shutdown_signal::with_test_signal_serialization(|| {
            // A pending signal would normally make the wait spin for the full
            // grace period waiting for an acknowledgement that will never come.
            crate::shutdown_signal::record_pending_termination_signal(
                signal_hook::consts::signal::SIGTERM,
            );

            // Without the latch this branch costs SIGNAL_SHUTDOWN_GRACE (2s).
            let started = std::time::Instant::now();
            EXPLICIT_TEARDOWN_STARTED.store(true, Ordering::SeqCst);
            assert!(wait_for_shutdown_ack());
            assert!(
                started.elapsed() < SIGNAL_SHUTDOWN_GRACE / 2,
                "ack must be immediate once explicit teardown started, took {:?}",
                started.elapsed()
            );

            EXPLICIT_TEARDOWN_STARTED.store(false, Ordering::SeqCst);
        });
    }

    /// CONTRACT (bd-kdn7n item 2): the terminal output lock is reentrant on
    /// the owning thread — the panic hook fires ON the render thread while it
    /// may already hold the lock mid-frame, so nested acquisition (teardown
    /// under a held lock) must not deadlock.
    #[test]
    fn output_lock_is_reentrant_within_thread() {
        let _outer = terminal_output_lock();
        // Would hang forever if the lock were not reentrant on the owning
        // thread: this call re-enters from "inside" the held lock.
        best_effort_cleanup_to(&mut Vec::new(), &TerminalCapabilities::modern());
    }

    /// CONTRACT (bd-kdn7n item 2): the lock excludes OTHER threads — a
    /// signal thread running teardown must not complete acquisition while a
    /// frame batch is in flight, and must proceed once it completes.
    #[test]
    fn output_lock_excludes_other_threads() {
        let held = terminal_output_lock();
        let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
        let worker = std::thread::spawn(move || {
            tx.send("started").expect("receiver alive");
            // Blocks until the main thread drops its guard.
            let _acquired = terminal_output_lock();
            tx.send("acquired").expect("receiver alive");
        });

        assert_eq!(rx.recv().expect("worker started"), "started");
        // The worker cannot have completed acquisition: it can only pass the
        // lock after this thread releases it, which has not happened yet.
        assert!(
            rx.try_recv().is_err(),
            "worker must still be blocked while the batch is held"
        );
        drop(held);
        assert_eq!(
            rx.recv().expect("worker acquired after release"),
            "acquired"
        );
        worker.join().expect("worker thread joins");
    }

    #[test]
    fn mouse_enable_excludes_any_event_mode() {
        assert!(
            !MOUSE_ENABLE_SEQ
                .windows(b"\x1b[?1003h".len())
                .any(|w| w == b"\x1b[?1003h"),
            "mouse enable must not include 1003 any-event mode"
        );
        assert!(
            MOUSE_ENABLE_SEQ
                .windows(b"\x1b[?1000h".len())
                .any(|w| w == b"\x1b[?1000h"),
            "mouse enable should include 1000 normal tracking"
        );
        assert!(
            MOUSE_ENABLE_SEQ
                .windows(b"\x1b[?1002h".len())
                .any(|w| w == b"\x1b[?1002h"),
            "mouse enable should include 1002 button-event tracking"
        );
        assert!(
            MOUSE_ENABLE_SEQ
                .windows(b"\x1b[?1006h".len())
                .any(|w| w == b"\x1b[?1006h"),
            "mouse enable should include 1006 SGR tracking"
        );
        let pos_1016l = MOUSE_ENABLE_SEQ
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mouse enable should clear 1016 before enabling SGR");
        let pos_1006h = MOUSE_ENABLE_SEQ
            .windows(b"\x1b[?1006h".len())
            .position(|w| w == b"\x1b[?1006h")
            .expect("mouse enable should include 1006 SGR mode");
        assert!(
            pos_1016l < pos_1006h,
            "1016l must be emitted before 1006h to preserve SGR mode on Ghostty-like terminals"
        );
    }

    #[test]
    fn mouse_enable_mux_safe_sequence_is_minimal() {
        let mux_caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .in_tmux(true)
            .build();
        assert_eq!(
            TerminalSession::mouse_enable_sequence_for_caps(&mux_caps),
            MOUSE_ENABLE_MUX_SAFE_SEQ
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE_SEQ
                .windows(b"\x1b[?1005l".len())
                .any(|w| w == b"\x1b[?1005l"),
            "mux-safe mouse enable must clear UTF-8 mouse encoding (1005)"
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE_SEQ
                .windows(b"\x1b[?1015l".len())
                .any(|w| w == b"\x1b[?1015l"),
            "mux-safe mouse enable must clear urxvt mouse encoding (1015)"
        );
        assert!(
            MOUSE_ENABLE_MUX_SAFE_SEQ
                .windows(b"\x1b[?1006h".len())
                .any(|w| w == b"\x1b[?1006h"),
            "mux-safe mouse enable must keep SGR mode"
        );
        assert!(
            !MOUSE_ENABLE_MUX_SAFE_SEQ
                .windows(b"\x1b[?1003h".len())
                .any(|w| w == b"\x1b[?1003h"),
            "mux-safe mouse enable must not include 1003 any-event mode"
        );
        let pos_1016l = MOUSE_ENABLE_MUX_SAFE_SEQ
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mux-safe mouse enable should clear 1016 before enabling SGR");
        let pos_1006h = MOUSE_ENABLE_MUX_SAFE_SEQ
            .windows(b"\x1b[?1006h".len())
            .position(|w| w == b"\x1b[?1006h")
            .expect("mux-safe mouse enable should include 1006 SGR mode");
        assert!(
            pos_1016l < pos_1006h,
            "mux-safe mouse enable must emit 1016l before 1006h to preserve SGR mode"
        );
    }

    #[test]
    fn mouse_disable_mux_safe_sequence_clears_1016() {
        let mux_caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .in_tmux(true)
            .build();
        assert_eq!(
            TerminalSession::mouse_disable_sequence_for_caps(&mux_caps),
            MOUSE_DISABLE_MUX_SAFE_SEQ
        );
        let pos_1016l = MOUSE_DISABLE_MUX_SAFE_SEQ
            .windows(b"\x1b[?1016l".len())
            .position(|w| w == b"\x1b[?1016l")
            .expect("mux-safe mouse disable should clear 1016");
        let pos_1006l = MOUSE_DISABLE_MUX_SAFE_SEQ
            .windows(b"\x1b[?1006l".len())
            .position(|w| w == b"\x1b[?1006l")
            .expect("mux-safe mouse disable should disable 1006");
        assert!(
            pos_1016l < pos_1006l,
            "mux-safe mouse disable should clear 1016 before dropping SGR mode"
        );
    }

    #[test]
    fn session_options_partial_config() {
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: false,
            bracketed_paste: true,
            ..Default::default()
        };
        assert!(opts.alternate_screen);
        assert!(!opts.mouse_capture);
        assert!(opts.bracketed_paste);
        assert!(!opts.focus_events);
        assert!(!opts.kitty_keyboard);
    }

    #[test]
    fn sanitize_session_options_disables_unsupported_capabilities() {
        let requested = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let caps = TerminalCapabilities::basic();
        let sanitized = sanitize_session_options(requested, &caps);

        assert!(sanitized.alternate_screen);
        assert!(!sanitized.mouse_capture);
        assert!(!sanitized.bracketed_paste);
        assert!(!sanitized.focus_events);
        assert!(!sanitized.kitty_keyboard);
    }

    #[test]
    fn sanitize_session_options_is_conservative_in_wezterm_mux() {
        let requested = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .bracketed_paste(true)
            .focus_events(true)
            .kitty_keyboard(true)
            .in_wezterm_mux(true)
            .build();
        let sanitized = sanitize_session_options(requested, &caps);

        assert!(sanitized.alternate_screen);
        assert!(sanitized.mouse_capture);
        assert!(sanitized.bracketed_paste);
        assert!(!sanitized.focus_events);
        assert!(!sanitized.kitty_keyboard);
    }

    #[test]
    fn sanitize_session_options_is_conservative_in_tmux() {
        let requested = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let caps = TerminalCapabilities::builder()
            .mouse_sgr(true)
            .bracketed_paste(true)
            .focus_events(true)
            .kitty_keyboard(true)
            .in_tmux(true)
            .build();
        let sanitized = sanitize_session_options(requested, &caps);

        assert!(sanitized.alternate_screen);
        assert!(sanitized.mouse_capture);
        assert!(sanitized.bracketed_paste);
        assert!(!sanitized.focus_events);
        assert!(!sanitized.kitty_keyboard);
    }

    #[cfg(unix)]
    enum ReaderMsg {
        Data(Vec<u8>),
        Eof,
        Err(std::io::Error),
    }

    #[cfg(unix)]
    fn read_until_pattern(
        rx: &mpsc::Receiver<ReaderMsg>,
        captured: &mut Vec<u8>,
        pattern: &[u8],
        timeout: Duration,
    ) -> std::io::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(Duration::from_millis(50));
            match rx.recv_timeout(wait) {
                Ok(ReaderMsg::Data(chunk)) => {
                    captured.extend_from_slice(&chunk);
                    if captured.windows(pattern.len()).any(|w| w == pattern) {
                        return Ok(());
                    }
                }
                Ok(ReaderMsg::Eof) => break,
                Ok(ReaderMsg::Err(err)) => return Err(err),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Err(std::io::Error::other(
            "timeout waiting for PTY output marker",
        ))
    }

    #[cfg(unix)]
    fn assert_contains_any(output: &[u8], options: &[&[u8]], label: &str) {
        let found = options
            .iter()
            .any(|needle| output.windows(needle.len()).any(|w| w == *needle));
        assert!(found, "expected cleanup sequence for {label}");
    }

    // -----------------------------------------------------------------------
    // Kitty keyboard escape helpers
    // -----------------------------------------------------------------------

    #[test]
    fn kitty_keyboard_enable_writes_correct_sequence() {
        let mut buf = Vec::new();
        TerminalSession::enable_kitty_keyboard(&mut buf).unwrap();
        assert_eq!(buf, b"\x1b[>15u");
    }

    #[test]
    fn kitty_keyboard_disable_writes_correct_sequence() {
        let mut buf = Vec::new();
        TerminalSession::disable_kitty_keyboard(&mut buf).unwrap();
        assert_eq!(buf, b"\x1b[<u");
    }

    #[test]
    fn kitty_keyboard_roundtrip_writes_both_sequences() {
        let mut buf = Vec::new();
        TerminalSession::enable_kitty_keyboard(&mut buf).unwrap();
        TerminalSession::disable_kitty_keyboard(&mut buf).unwrap();
        assert_eq!(buf, b"\x1b[>15u\x1b[<u");
    }

    // -----------------------------------------------------------------------
    // SessionOptions exhaustive
    // -----------------------------------------------------------------------

    #[test]
    fn session_options_all_enabled() {
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        assert!(opts.alternate_screen);
        assert!(opts.mouse_capture);
        assert!(opts.bracketed_paste);
        assert!(opts.focus_events);
        assert!(opts.kitty_keyboard);
    }

    #[test]
    fn session_options_debug_contains_all_fields() {
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: false,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let debug = format!("{opts:?}");
        assert!(debug.contains("alternate_screen: true"), "{debug}");
        assert!(debug.contains("mouse_capture: false"), "{debug}");
        assert!(debug.contains("bracketed_paste: true"), "{debug}");
        assert!(debug.contains("focus_events: false"), "{debug}");
        assert!(debug.contains("kitty_keyboard: true"), "{debug}");
        assert!(debug.contains("intercept_signals: true"), "{debug}");
    }

    #[test]
    fn session_options_clone_independence() {
        let opts = SessionOptions {
            alternate_screen: true,
            ..Default::default()
        };
        let mut cloned = opts.clone();
        cloned.alternate_screen = false;
        // Original unchanged
        assert!(opts.alternate_screen);
        assert!(!cloned.alternate_screen);
    }

    // -----------------------------------------------------------------------
    // new_for_tests construction (requires test-helpers feature)
    // -----------------------------------------------------------------------

    #[cfg(feature = "test-helpers")]
    #[test]
    fn new_for_tests_default_options() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        assert!(!session.mouse_capture_enabled());
        assert!(!session.alternate_screen_enabled);
        assert!(!session.mouse_enabled);
        assert!(!session.bracketed_paste_enabled);
        assert!(!session.focus_events_enabled);
        assert!(!session.kitty_keyboard_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn new_for_tests_preserves_options() {
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let session = TerminalSession::new_for_tests(opts).unwrap();
        let stored = session.options();
        assert!(stored.alternate_screen);
        assert!(stored.mouse_capture);
        assert!(stored.bracketed_paste);
        assert!(stored.focus_events);
        assert!(stored.kitty_keyboard);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn new_for_tests_flags_all_false_regardless_of_options() {
        // Even if options request features, new_for_tests skips enabling them
        let opts = SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        };
        let session = TerminalSession::new_for_tests(opts).unwrap();
        // Flags track *actual* enabled state, not *requested* state
        assert!(!session.alternate_screen_enabled);
        assert!(!session.mouse_enabled);
        assert!(!session.bracketed_paste_enabled);
        assert!(!session.focus_events_enabled);
        assert!(!session.kitty_keyboard_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn new_for_tests_allows_multiple_sessions() {
        let _a = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let _b = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn new_for_tests_marks_session_headless() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        assert!(session.headless);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn mouse_capture_enabled_getter() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        assert!(!session.mouse_capture_enabled());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn options_getter_returns_session_options() {
        let opts = SessionOptions {
            mouse_capture: true,
            focus_events: true,
            ..Default::default()
        };
        let session = TerminalSession::new_for_tests(opts).unwrap();
        assert!(session.options().mouse_capture);
        assert!(session.options().focus_events);
        assert!(!session.options().alternate_screen);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn set_mouse_capture_idempotent_disable() {
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        // Already disabled - should be no-op
        assert!(!session.mouse_capture_enabled());
        session.set_mouse_capture(false).unwrap();
        assert!(!session.mouse_capture_enabled());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn set_mouse_capture_enable_then_idempotent_enable() {
        let _g = crate::capability_override::push_override(
            crate::capability_override::CapabilityOverride::modern(),
        );
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        // Enable mouse
        session.set_mouse_capture(true).unwrap();
        assert!(session.mouse_capture_enabled());
        assert!(session.options().mouse_capture);
        // Enable again - idempotent
        session.set_mouse_capture(true).unwrap();
        assert!(session.mouse_capture_enabled());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn set_mouse_capture_toggle_roundtrip() {
        let _g = crate::capability_override::push_override(
            crate::capability_override::CapabilityOverride::modern(),
        );
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        assert!(!session.mouse_capture_enabled());

        session.set_mouse_capture(true).unwrap();
        assert!(session.mouse_capture_enabled());
        assert!(session.options().mouse_capture);

        session.set_mouse_capture(false).unwrap();
        assert!(!session.mouse_capture_enabled());
        assert!(!session.options().mouse_capture);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn set_mouse_capture_multiple_toggles() {
        let _g = crate::capability_override::push_override(
            crate::capability_override::CapabilityOverride::modern(),
        );
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        for _ in 0..5 {
            session.set_mouse_capture(true).unwrap();
            assert!(session.mouse_capture_enabled());
            session.set_mouse_capture(false).unwrap();
            assert!(!session.mouse_capture_enabled());
        }
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn cleanup_clears_all_flags() {
        let mut session = TerminalSession::new_for_tests(SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        })
        .unwrap();
        // Manually set flags to simulate features being enabled
        session.alternate_screen_enabled = true;
        session.mouse_enabled = true;
        session.bracketed_paste_enabled = true;
        session.focus_events_enabled = true;
        session.kitty_keyboard_enabled = true;

        session.cleanup();

        assert!(!session.alternate_screen_enabled);
        assert!(!session.mouse_enabled);
        assert!(!session.bracketed_paste_enabled);
        assert!(!session.focus_events_enabled);
        assert!(!session.kitty_keyboard_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn cleanup_is_idempotent() {
        let mut session = TerminalSession::new_for_tests(SessionOptions {
            mouse_capture: true,
            ..Default::default()
        })
        .unwrap();
        session.mouse_enabled = true;

        session.cleanup();
        assert!(!session.mouse_enabled);
        // Second cleanup should be safe (no-op since flags already cleared)
        session.cleanup();
        assert!(!session.mouse_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn cleanup_only_disables_enabled_features() {
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        // Only enable mouse, leave others off
        session.mouse_enabled = true;
        // Cleanup should handle partial state gracefully
        session.cleanup();
        assert!(!session.mouse_enabled);
        assert!(!session.alternate_screen_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn cleanup_headless_session_clears_feature_flags() {
        let mut session = TerminalSession::new_for_tests(SessionOptions {
            alternate_screen: true,
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            intercept_signals: true,
        })
        .unwrap();
        session.alternate_screen_enabled = true;
        session.mouse_enabled = true;
        session.bracketed_paste_enabled = true;
        session.focus_events_enabled = true;
        session.kitty_keyboard_enabled = true;

        session.cleanup();

        assert!(!session.alternate_screen_enabled);
        assert!(!session.mouse_enabled);
        assert!(!session.bracketed_paste_enabled);
        assert!(!session.focus_events_enabled);
        assert!(!session.kitty_keyboard_enabled);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn session_debug_format() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let debug = format!("{session:?}");
        assert!(debug.contains("TerminalSession"), "{debug}");
        assert!(debug.contains("mouse_enabled"), "{debug}");
        assert!(debug.contains("alternate_screen_enabled"), "{debug}");
    }

    #[test]
    fn panic_cleanup_suppression_scope_restores_state() {
        assert!(
            !panic_cleanup_suppressed(),
            "suppression should start disabled"
        );
        with_panic_cleanup_suppressed(|| {
            if cfg!(panic = "abort") {
                assert!(
                    !panic_cleanup_suppressed(),
                    "abort profile must not suppress panic cleanup"
                );
                return;
            }
            assert!(panic_cleanup_suppressed(), "suppression should be enabled");
            with_panic_cleanup_suppressed(|| {
                assert!(
                    panic_cleanup_suppressed(),
                    "nested suppression should remain enabled"
                );
            });
            assert!(
                panic_cleanup_suppressed(),
                "outer suppression should still be enabled after nested scope"
            );
        });
        assert!(
            !panic_cleanup_suppressed(),
            "suppression should be disabled after scope exits"
        );
    }

    // -----------------------------------------------------------------------
    // PTY integration tests
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn terminal_session_panic_cleanup_idempotent() {
        const MARKER: &[u8] = b"PANIC_CAUGHT";
        const TEST_NAME: &str =
            "terminal_session::tests::terminal_session_panic_cleanup_idempotent";
        const ALT_SCREEN_EXIT_SEQS: &[&[u8]] = &[b"\x1b[?1049l", b"\x1b[?1047l"];
        const MOUSE_DISABLE_SEQS: &[&[u8]] = &[
            b"\x1b[?1000l\x1b[?1002l\x1b[?1006l",
            b"\x1b[?1000;1002;1006l",
            b"\x1b[?1000;1002l",
            b"\x1b[?1000l",
        ];
        const BRACKETED_PASTE_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[?2004l"];
        const FOCUS_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[?1004l"];
        const KITTY_DISABLE_SEQS: &[&[u8]] = &[b"\x1b[<u"];
        const CURSOR_SHOW_SEQS: &[&[u8]] = &[b"\x1b[?25h"];
        const SCROLL_REGION_RESET_SEQS: &[&[u8]] = &[b"\x1b[r"];
        const STYLE_RESET_SEQS: &[&[u8]] = &[b"\x1b[0m"];

        if std::env::var("FTUI_CORE_PANIC_CHILD").is_ok() {
            let _ = std::panic::catch_unwind(|| {
                let _session = TerminalSession::new(SessionOptions {
                    alternate_screen: true,
                    mouse_capture: true,
                    bracketed_paste: true,
                    focus_events: true,
                    kitty_keyboard: true,
                    intercept_signals: true,
                })
                .expect("TerminalSession::new should succeed in PTY");
                panic!("intentional panic to exercise cleanup");
            });

            // The panic hook + Drop will have already attempted cleanup; call again to
            // verify idempotence when cleanup paths run multiple times.
            best_effort_cleanup_for_exit();

            let _ = io::stdout().write_all(MARKER);
            let _ = io::stdout().flush();
            return;
        }

        let exe = std::env::current_exe().expect("current_exe");
        let mut cmd = CommandBuilder::new(exe);
        cmd.args(["--exact", TEST_NAME, "--nocapture"]);
        cmd.env("FTUI_CORE_PANIC_CHILD", "1");
        cmd.env("RUST_BACKTRACE", "0");
        cmd.env("TERM", "xterm-256color");
        cmd.env("FTUI_TEST_PROFILE", "modern");
        cmd.env("TERM_PROGRAM", "WezTerm");
        cmd.env_remove("TMUX");
        cmd.env_remove("STY");
        cmd.env_remove("ZELLIJ");
        cmd.env_remove("WEZTERM_PANE");

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut child = pair.slave.spawn_command(cmd).expect("spawn PTY child");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        let _writer = pair.master.take_writer().expect("take PTY writer");

        let (tx, rx) = mpsc::channel::<ReaderMsg>();
        let reader_thread = thread::spawn(move || {
            let mut buf = [0u8; 4096];
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

        let mut captured = Vec::new();
        read_until_pattern(&rx, &mut captured, MARKER, Duration::from_secs(5))
            .expect("expected marker from child");

        let status = child.wait().expect("child wait");
        let _ = reader_thread.join();

        assert!(status.success(), "child should exit successfully");
        assert!(
            captured.windows(MARKER.len()).any(|w| w == MARKER),
            "expected panic marker in PTY output"
        );
        assert_contains_any(&captured, ALT_SCREEN_EXIT_SEQS, "alt-screen exit");
        assert_contains_any(&captured, MOUSE_DISABLE_SEQS, "mouse disable");
        assert_contains_any(
            &captured,
            BRACKETED_PASTE_DISABLE_SEQS,
            "bracketed paste disable",
        );
        assert_contains_any(&captured, FOCUS_DISABLE_SEQS, "focus disable");
        assert_contains_any(&captured, KITTY_DISABLE_SEQS, "kitty disable");
        assert_contains_any(&captured, CURSOR_SHOW_SEQS, "cursor show");
        assert_contains_any(&captured, SCROLL_REGION_RESET_SEQS, "scroll-region reset");
        assert_contains_any(&captured, STYLE_RESET_SEQS, "style reset");
    }

    #[cfg(unix)]
    #[test]
    fn terminal_session_enforces_single_active_session() {
        const MARKER: &[u8] = b"EXCLUSIVITY_OK";
        const TEST_NAME: &str =
            "terminal_session::tests::terminal_session_enforces_single_active_session";

        if std::env::var("FTUI_CORE_EXCLUSIVITY_CHILD").is_ok() {
            let session = TerminalSession::new(SessionOptions::default())
                .expect("TerminalSession::new should succeed in PTY");
            let err = TerminalSession::new(SessionOptions::default())
                .expect_err("second TerminalSession::new should be rejected");
            let msg = err.to_string();
            assert!(
                msg.contains("already active"),
                "unexpected error message: {msg}"
            );
            drop(session);

            let _session2 = TerminalSession::new(SessionOptions::default())
                .expect("TerminalSession::new should succeed after previous session dropped");

            let _ = io::stdout().write_all(MARKER);
            let _ = io::stdout().flush();
            return;
        }

        let exe = std::env::current_exe().expect("current_exe");
        let mut cmd = CommandBuilder::new(exe);
        cmd.args(["--exact", TEST_NAME, "--nocapture"]);
        cmd.env("FTUI_CORE_EXCLUSIVITY_CHILD", "1");
        cmd.env("RUST_BACKTRACE", "0");
        cmd.env("TERM", "xterm-256color");
        cmd.env("TERM_PROGRAM", "WezTerm");
        cmd.env_remove("TMUX");
        cmd.env_remove("STY");
        cmd.env_remove("ZELLIJ");
        cmd.env_remove("WEZTERM_PANE");

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut child = pair.slave.spawn_command(cmd).expect("spawn PTY child");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        let _writer = pair.master.take_writer().expect("take PTY writer");

        let (tx, rx) = mpsc::channel::<ReaderMsg>();
        let reader_thread = thread::spawn(move || {
            let mut buf = [0u8; 4096];
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

        let mut captured = Vec::new();
        read_until_pattern(&rx, &mut captured, MARKER, Duration::from_secs(5))
            .expect("expected marker from child");

        let status = child.wait().expect("child wait");
        let _ = reader_thread.join();

        assert!(status.success(), "child should exit successfully");
        assert!(
            captured.windows(MARKER.len()).any(|w| w == MARKER),
            "expected marker in PTY output"
        );
    }

    // -----------------------------------------------------------------------
    // Cx helper function tests
    // -----------------------------------------------------------------------

    #[test]
    fn to_std_duration_converts_correctly() {
        let d = web_time::Duration::from_millis(1234);
        let std_d = super::to_std_duration(d);
        assert_eq!(std_d, Duration::from_millis(1234));
    }

    #[test]
    fn to_std_duration_zero() {
        let d = web_time::Duration::from_secs(0);
        let std_d = super::to_std_duration(d);
        assert_eq!(std_d, Duration::ZERO);
    }

    #[test]
    fn to_std_duration_large_value() {
        let d = web_time::Duration::from_secs(86400);
        let std_d = super::to_std_duration(d);
        assert_eq!(std_d, Duration::from_secs(86400));
    }

    #[test]
    fn cx_deadline_remaining_us_no_deadline() {
        let (cx, _ctrl) = crate::cx::Cx::background();
        let remaining = super::cx_deadline_remaining_us(&cx);
        assert_eq!(remaining, u64::MAX);
    }

    #[test]
    fn cx_deadline_remaining_us_with_deadline() {
        let (cx, _ctrl) = crate::cx::Cx::with_deadline(web_time::Duration::from_millis(500));
        let remaining = super::cx_deadline_remaining_us(&cx);
        // Should be approximately 500_000 us (allow some elapsed time)
        assert!(remaining <= 500_000, "remaining={remaining}");
        assert!(remaining > 400_000, "remaining={remaining}");
    }

    #[test]
    fn cx_deadline_remaining_us_cancelled() {
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        // No deadline means u64::MAX even when cancelled (deadline is separate from cancellation)
        assert_eq!(super::cx_deadline_remaining_us(&cx), u64::MAX);
    }

    #[test]
    fn cx_deadline_remaining_us_expired() {
        let (cx, _ctrl) = crate::cx::Cx::with_deadline(web_time::Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        let remaining = super::cx_deadline_remaining_us(&cx);
        assert_eq!(remaining, 0);
    }

    // -----------------------------------------------------------------------
    // Metrics function tests
    // -----------------------------------------------------------------------

    #[test]
    fn terminal_io_stats_functions_return_tuples() {
        // Verify the metric accessor functions return without panicking.
        let (_sum, _count) = terminal_io_read_stats();
        let (_sum_w, _count_w) = terminal_io_write_stats();
        let (_sum_f, _count_f) = terminal_io_flush_stats();
    }

    #[test]
    fn terminal_io_metrics_counters_are_monotonic() {
        // Read initial state
        let (_, count_before) = terminal_io_read_stats();
        // Counters are global and shared across tests — just verify they don't decrease
        let (_, count_after) = terminal_io_read_stats();
        assert!(count_after >= count_before);
    }

    // -----------------------------------------------------------------------
    // Cx-aware method tests (using test-helpers feature)
    // -----------------------------------------------------------------------

    #[cfg(feature = "test-helpers")]
    #[test]
    fn poll_event_cx_returns_false_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        let result = session.poll_event_cx(Duration::from_secs(10), &cx);
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "cancelled cx should return false immediately"
        );
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn poll_event_cx_returns_false_when_expired() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, _ctrl) = crate::cx::Cx::with_deadline(web_time::Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        let result = session.poll_event_cx(Duration::from_secs(10), &cx);
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "expired cx should return false immediately"
        );
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn read_event_cx_returns_none_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        let result = session.read_event_cx(&cx);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "cancelled cx should return None");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn read_event_cx_returns_none_when_expired() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, _ctrl) = crate::cx::Cx::with_deadline(web_time::Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        let result = session.read_event_cx(&cx);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "expired cx should return None");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn show_cursor_cx_noop_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        assert!(session.show_cursor_cx(&cx).is_ok());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn hide_cursor_cx_noop_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        assert!(session.hide_cursor_cx(&cx).is_ok());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn flush_cx_noop_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        assert!(session.flush_cx(&cx).is_ok());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn size_cx_returns_minimum_when_cancelled() {
        let session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        let (w, h) = session.size_cx(&cx).unwrap();
        assert!(w >= 2, "width={w}");
        assert!(h >= 2, "height={h}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn set_mouse_capture_cx_noop_when_cancelled() {
        let mut session = TerminalSession::new_for_tests(SessionOptions::default()).unwrap();
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        assert!(session.set_mouse_capture_cx(true, &cx).is_ok());
        // Mouse should NOT be enabled since cx was cancelled
        assert!(!session.mouse_capture_enabled());
    }

    // -----------------------------------------------------------------------
    // Cx-aware methods with Lab clock (deterministic timing)
    // -----------------------------------------------------------------------

    #[test]
    fn cx_lab_deadline_remaining_us_deterministic() {
        let clock = crate::cx::LabClock::new();
        let (cx, _ctrl) =
            crate::cx::Cx::lab_with_deadline(&clock, web_time::Duration::from_millis(100));
        // Lab clock hasn't advanced, so remaining should be ~100ms
        let remaining = super::cx_deadline_remaining_us(&cx);
        assert!(remaining <= 100_000, "remaining={remaining}");
        assert!(remaining > 90_000, "remaining={remaining}");

        // Advance clock by 50ms
        clock.advance(web_time::Duration::from_millis(50));
        let remaining = super::cx_deadline_remaining_us(&cx);
        assert!(remaining <= 50_000, "remaining={remaining}");
        assert!(remaining > 40_000, "remaining={remaining}");
    }

    #[test]
    fn size_retry_delay_uses_default_window_without_deadline() {
        let (cx, _ctrl) = crate::cx::Cx::background();
        assert_eq!(super::size_retry_delay(&cx), Some(SIZE_RETRY_DELAY));
    }

    #[test]
    fn size_retry_delay_skips_when_cancelled() {
        let (cx, ctrl) = crate::cx::Cx::background();
        ctrl.cancel();
        assert_eq!(super::size_retry_delay(&cx), None);
    }

    #[test]
    fn size_retry_delay_skips_when_deadline_cannot_cover_retry() {
        let clock = crate::cx::LabClock::new();
        let (cx, _ctrl) =
            crate::cx::Cx::lab_with_deadline(&clock, web_time::Duration::from_millis(10));
        assert_eq!(super::size_retry_delay(&cx), None);
    }

    #[test]
    fn size_retry_delay_allows_retry_when_budget_exceeds_delay() {
        let clock = crate::cx::LabClock::new();
        let (cx, _ctrl) =
            crate::cx::Cx::lab_with_deadline(&clock, web_time::Duration::from_millis(25));
        assert_eq!(super::size_retry_delay(&cx), Some(SIZE_RETRY_DELAY));
    }

    #[test]
    fn pending_termination_signal_round_trip() {
        crate::shutdown_signal::with_test_signal_serialization(|| {
            crate::shutdown_signal::clear_pending_termination_signal();
            assert_eq!(crate::shutdown_signal::pending_termination_signal(), None);

            crate::shutdown_signal::record_pending_termination_signal(2);
            assert_eq!(
                crate::shutdown_signal::pending_termination_signal(),
                Some(2)
            );

            // First signal wins until explicitly cleared.
            crate::shutdown_signal::record_pending_termination_signal(15);
            assert_eq!(
                crate::shutdown_signal::pending_termination_signal(),
                Some(2)
            );

            crate::shutdown_signal::clear_pending_termination_signal();
            assert_eq!(crate::shutdown_signal::pending_termination_signal(), None);
        });
    }
}
