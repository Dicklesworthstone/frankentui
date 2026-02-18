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
//! | Mouse (SGR) | `CSI ? 1000 h` + `CSI ? 1002 h` + `CSI ? 1006 h` | `CSI ? 1000 l` + `CSI ? 1002 l` + `CSI ? 1006 l` |
//! | Bracketed paste | `CSI ? 2004 h` | `CSI ? 2004 l` |
//! | Focus events | `CSI ? 1004 h` | `CSI ? 1004 l` |
//! | Kitty keyboard | `CSI > 15 u` | `CSI < u` |
//! | Show cursor | `CSI ? 25 h` | `CSI ? 25 l` |
//! | Reset style | `CSI 0 m` | N/A |
//!
//! # Cleanup Order
//!
//! On drop, cleanup happens in reverse order of enabling:
//! 1. Disable kitty keyboard (if enabled)
//! 2. Disable focus events (if enabled)
//! 3. Disable bracketed paste (if enabled)
//! 4. Disable mouse capture (if enabled)
//! 5. Show cursor (always)
//! 6. Leave alternate screen (if enabled)
//! 7. Exit raw mode (always)
//! 8. Flush stdout
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

/// Compute remaining microseconds for Cx, or `u64::MAX` if no deadline.
#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
fn cx_deadline_remaining_us(cx: &crate::cx::Cx) -> u64 {
    cx.remaining()
        .map(|r| r.as_micros().min(u64::MAX as u128) as u64)
        .unwrap_or(u64::MAX)
}

const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";
const SYNC_END: &[u8] = b"\x1b[?2026l";
const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";
const MOUSE_ENABLE_SEQ: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_DISABLE_SEQ: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1006l";

#[inline]
fn should_emit_sync_cleanup() -> bool {
    crate::terminal_capabilities::TerminalCapabilities::with_overrides().use_sync_output()
}

static TERMINAL_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

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
use signal_hook::consts::signal::{SIGINT, SIGTERM, SIGWINCH};
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
#[derive(Debug, Clone, Default)]
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

    /// Enable mouse capture with SGR encoding (`CSI ? 1000 h` + `CSI ? 1002 h` + `CSI ? 1006 h`).
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
    /// Enter raw mode and optionally enable additional features.
    ///
    /// # Errors
    ///
    /// Returns an error if raw mode cannot be enabled.
    pub fn new(options: SessionOptions) -> io::Result<Self> {
        install_panic_hook();

        let session_lock = SessionLock::acquire()?;

        // Create signal guard before raw mode so that a failure here
        // does not leave the terminal in raw mode (the struct would never
        // be fully constructed, so Drop would not run).
        #[cfg(unix)]
        let signal_guard = Some(SignalGuard::new()?);

        // Enter raw mode
        crossterm::terminal::enable_raw_mode()?;
        #[cfg(feature = "tracing")]
        tracing::info!("terminal raw mode enabled");

        let mut session = Self {
            session_lock: Some(session_lock),
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
            stdout.write_all(MOUSE_ENABLE_SEQ)?;
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

    /// Create a session for tests without touching the real terminal.
    ///
    /// This skips raw mode and feature toggles, allowing headless tests
    /// to construct `TerminalSession` safely.
    #[cfg(feature = "test-helpers")]
    pub fn new_for_tests(options: SessionOptions) -> io::Result<Self> {
        install_panic_hook();
        #[cfg(unix)]
        let signal_guard = None;

        Ok(Self {
            session_lock: None,
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
        std::thread::sleep(Duration::from_millis(10));
        let (w2, h2) = crossterm::terminal::size()?;
        if w2 > 1 && h2 > 1 {
            return Ok((w2, h2));
        }

        // Ensure minimum viable size to prevent downstream panics in buffer allocation
        // and layout calculations. 2x2 is the absolute minimum for a functional TUI.
        let final_w = w.max(2);
        let final_h = h.max(2);
        Ok((final_w, final_h))
    }

    /// Poll for an event with a timeout.
    ///
    /// Returns `Ok(true)` if an event is available, `Ok(false)` if timeout.
    pub fn poll_event(&self, timeout: std::time::Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    /// Poll for an event, respecting the [`Cx`] deadline and cancellation.
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

    /// Read the next event, respecting the [`Cx`] deadline and cancellation.
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
        crossterm::execute!(io::stdout(), crossterm::cursor::Show)
    }

    /// Show the cursor, respecting the [`Cx`] deadline and cancellation.
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
        crossterm::execute!(io::stdout(), crossterm::cursor::Hide)
    }

    /// Hide the cursor, respecting the [`Cx`] deadline and cancellation.
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
        if enabled == self.mouse_enabled {
            self.options.mouse_capture = enabled;
            return Ok(());
        }

        let mut stdout = io::stdout();
        if enabled {
            stdout.write_all(MOUSE_ENABLE_SEQ)?;
            stdout.flush()?;
            self.mouse_enabled = true;
            self.options.mouse_capture = true;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture enabled (runtime toggle)");
        } else {
            stdout.write_all(MOUSE_DISABLE_SEQ)?;
            stdout.flush()?;
            self.mouse_enabled = false;
            self.options.mouse_capture = false;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture disabled (runtime toggle)");
        }

        Ok(())
    }

    /// Enable or disable terminal mouse capture, respecting the [`Cx`] deadline.
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
        if enabled == self.mouse_enabled {
            self.options.mouse_capture = enabled;
            return Ok(());
        }
        let start = web_time::Instant::now();
        let mut stdout = io::stdout();
        let result = if enabled {
            let r = stdout
                .write_all(MOUSE_ENABLE_SEQ)
                .and_then(|_| stdout.flush());
            if r.is_ok() {
                self.mouse_enabled = true;
                self.options.mouse_capture = true;
            }
            r
        } else {
            let r = stdout
                .write_all(MOUSE_DISABLE_SEQ)
                .and_then(|_| stdout.flush());
            if r.is_ok() {
                self.mouse_enabled = false;
                self.options.mouse_capture = false;
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

    /// Query terminal size, respecting the [`Cx`] deadline and cancellation.
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
        // Skip retry delay if Cx is running out of time.
        if cx.is_done() {
            return Ok((w.max(2), h.max(2)));
        }
        std::thread::sleep(Duration::from_millis(10));
        let (w2, h2) = crossterm::terminal::size()?;
        if w2 > 1 && h2 > 1 {
            return Ok((w2, h2));
        }
        Ok((w.max(2), h.max(2)))
    }

    /// Flush stdout, respecting the [`Cx`] deadline and cancellation.
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
        #[cfg(unix)]
        let _ = self.signal_guard.take();

        let mut stdout = io::stdout();

        // Normal session teardown should not emit a standalone DEC ?2026l.
        // Frame bracketing is owned by the renderer/writer and closed there.

        // Reset scroll region (critical for inline mode recovery)
        let _ = stdout.write_all(RESET_SCROLL_REGION);

        // Disable features in reverse order of enabling
        if self.kitty_keyboard_enabled {
            let _ = Self::disable_kitty_keyboard(&mut stdout);
            self.kitty_keyboard_enabled = false;
            #[cfg(feature = "tracing")]
            tracing::info!("kitty keyboard disabled");
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
            let _ = stdout.write_all(MOUSE_DISABLE_SEQ);
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
            if !panic_cleanup_suppressed() {
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

    // End synchronized output first to ensure any buffered content (like panic messages)
    // is flushed to the terminal.
    if should_emit_sync_cleanup() {
        let _ = stdout.write_all(SYNC_END);
    }
    let _ = stdout.write_all(RESET_SCROLL_REGION);

    let _ = TerminalSession::disable_kitty_keyboard(&mut stdout);
    let _ = crossterm::execute!(stdout, crossterm::event::DisableFocusChange);
    let _ = crossterm::execute!(stdout, crossterm::event::DisableBracketedPaste);
    let _ = stdout.write_all(MOUSE_DISABLE_SEQ);
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
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGWINCH]).map_err(io::Error::other)?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            for signal in signals.forever() {
                match signal {
                    SIGWINCH => {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("SIGWINCH received");
                    }
                    SIGINT | SIGTERM => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("termination signal received, cleaning up");
                        best_effort_cleanup();
                        std::process::exit(128 + signal);
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
        };
        let debug = format!("{opts:?}");
        assert!(debug.contains("alternate_screen: true"), "{debug}");
        assert!(debug.contains("mouse_capture: false"), "{debug}");
        assert!(debug.contains("bracketed_paste: true"), "{debug}");
        assert!(debug.contains("focus_events: false"), "{debug}");
        assert!(debug.contains("kitty_keyboard: true"), "{debug}");
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
    // Escape sequence constants
    // -----------------------------------------------------------------------

    #[test]
    fn sync_end_sequence_is_correct() {
        assert_eq!(SYNC_END, b"\x1b[?2026l");
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

        if std::env::var("FTUI_CORE_PANIC_CHILD").is_ok() {
            let _ = std::panic::catch_unwind(|| {
                let _session = TerminalSession::new(SessionOptions {
                    alternate_screen: true,
                    mouse_capture: true,
                    bracketed_paste: true,
                    focus_events: true,
                    kitty_keyboard: true,
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
}
