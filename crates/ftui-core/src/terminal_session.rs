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
//! | Mouse (SGR) | `CSI ? 1000;1002;1006 h` | `CSI ? 1000;1002;1006 l` |
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

use std::env;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::time::Duration;

use crate::event::Event;

const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";
const SYNC_END: &[u8] = b"\x1b[?2026l";

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

    /// Enable mouse capture with SGR encoding (`CSI ? 1000;1002;1006 h`).
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
            session.alternate_screen_enabled = true;
            #[cfg(feature = "tracing")]
            tracing::info!("alternate screen enabled (with clear)");
        }

        if options.mouse_capture {
            crossterm::execute!(stdout, crossterm::event::EnableMouseCapture)?;
            session.mouse_enabled = true;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture enabled");
        }

        if options.bracketed_paste {
            crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste)?;
            session.bracketed_paste_enabled = true;
            #[cfg(feature = "tracing")]
            tracing::info!("bracketed paste enabled");
        }

        if options.focus_events {
            crossterm::execute!(stdout, crossterm::event::EnableFocusChange)?;
            session.focus_events_enabled = true;
            #[cfg(feature = "tracing")]
            tracing::info!("focus events enabled");
        }

        if options.kitty_keyboard {
            Self::enable_kitty_keyboard(&mut stdout)?;
            session.kitty_keyboard_enabled = true;
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

    /// Read the next event (blocking until available).
    ///
    /// Returns `Ok(None)` if the event cannot be represented by the
    /// ftui canonical event types (e.g. unsupported key codes).
    pub fn read_event(&self) -> io::Result<Option<Event>> {
        let event = crossterm::event::read()?;
        Ok(Event::from_crossterm(event))
    }

    /// Show the cursor.
    pub fn show_cursor(&self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Show)
    }

    /// Hide the cursor.
    pub fn hide_cursor(&self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Hide)
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
            crossterm::execute!(stdout, crossterm::event::EnableMouseCapture)?;
            self.mouse_enabled = true;
            self.options.mouse_capture = true;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture enabled (runtime toggle)");
        } else {
            crossterm::execute!(stdout, crossterm::event::DisableMouseCapture)?;
            self.mouse_enabled = false;
            self.options.mouse_capture = false;
            #[cfg(feature = "tracing")]
            tracing::info!("mouse capture disabled (runtime toggle)");
        }

        Ok(())
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

        // End synchronized output first to ensure terminal updates resume
        let _ = stdout.write_all(SYNC_END);

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
            let _ = crossterm::execute!(stdout, crossterm::event::DisableMouseCapture);
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
            best_effort_cleanup();
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
    let _ = stdout.write_all(SYNC_END);

    let _ = TerminalSession::disable_kitty_keyboard(&mut stdout);
    let _ = crossterm::execute!(stdout, crossterm::event::DisableFocusChange);
    let _ = crossterm::execute!(stdout, crossterm::event::DisableBracketedPaste);
    let _ = crossterm::execute!(stdout, crossterm::event::DisableMouseCapture);
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
}
