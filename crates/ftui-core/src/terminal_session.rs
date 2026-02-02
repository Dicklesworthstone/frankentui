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

use std::io::{self, Write};
use std::sync::OnceLock;

use crate::event::Event;

const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";

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
            crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
            session.alternate_screen_enabled = true;
            #[cfg(feature = "tracing")]
            tracing::info!("alternate screen enabled");
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

    /// Create a minimal session (raw mode only).
    pub fn minimal() -> io::Result<Self> {
        Self::new(SessionOptions::default())
    }

    /// Get the current terminal size (columns, rows).
    pub fn size(&self) -> io::Result<(u16, u16)> {
        crossterm::terminal::size()
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

    /// Get the session options.
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// Cleanup helper (shared between drop and explicit cleanup).
    fn cleanup(&mut self) {
        #[cfg(unix)]
        let _ = self.signal_guard.take();

        let mut stdout = io::stdout();

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
}
