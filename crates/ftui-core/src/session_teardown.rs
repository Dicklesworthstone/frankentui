#![forbid(unsafe_code)]

//! Unified terminal session teardown plan, escape sequences, and panic/signal hooks.
//!
//! Shared by both the Crossterm backend ([`crate::terminal_session`]) and the native
//! Unix backend (`ftui-tty`).
//!
//! # Canonical Teardown Sequence Order
//! 1. `SYNC_END` (`\x1b[?2026l`) if requested
//! 2. Scroll region reset (`\x1b7\x1b[r\x1b8`) if requested
//! 3. Style / SGR reset (`\x1b[0m`) if requested
//! 4. Kitty keyboard pop (`\x1b[<u`) if requested
//! 5. Focus tracking disable (`\x1b[?1004l`) if requested
//! 6. Bracketed paste disable (`\x1b[?2004l`) if requested
//! 7. Mouse tracking disable (canonical SGR or mux-safe) if requested
//! 8. Show cursor (`\x1b[?25h`) if requested
//! 9. Leave alternate screen (`\x1b[?1049l`) if requested
//!
//! Note: Raw-mode restore is NOT part of the byte stream (it is an ioctl / termios
//! call) and is always performed last, after byte emission and stdout flush.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::terminal_capabilities::TerminalCapabilities;

/// Canonical escape sequence byte constants for terminal teardown and lifecycle.
pub mod seq {
    /// Save cursor position (DECSC).
    pub const DECSC: &[u8] = b"\x1b7";

    /// Restore cursor position (DECRC).
    pub const DECRC: &[u8] = b"\x1b8";

    /// Reset scroll region / margins to full screen (DECSTBM).
    pub const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";

    /// Reset text style / SGR attributes to default (SGR 0).
    pub const RESET_STYLE: &[u8] = b"\x1b[0m";

    /// Alias for `RESET_STYLE` for compatibility.
    pub const SGR_RESET: &[u8] = RESET_STYLE;

    /// End synchronized output (DEC 2026).
    pub const SYNC_END: &[u8] = b"\x1b[?2026l";

    /// Kitty keyboard protocol push flags.
    pub const KITTY_KEYBOARD_ENABLE: &[u8] = b"\x1b[>15u";

    /// Kitty keyboard protocol pop flags.
    pub const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";

    /// Disable focus event reporting.
    pub const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";

    /// Enable focus event reporting.
    pub const FOCUS_ENABLE: &[u8] = b"\x1b[?1004h";

    /// Disable bracketed paste mode.
    pub const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

    /// Enable bracketed paste mode.
    pub const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";

    /// Canonical SGR mouse disable sequence.
    pub const MOUSE_DISABLE: &[u8] = b"\x1b[?1000;1002;1006l\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l";

    /// Conservative mouse disable sequence for mux sessions and panic paths.
    pub const MOUSE_DISABLE_MUX_SAFE: &[u8] =
        b"\x1b[?1016l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1001l\x1b[?1005l\x1b[?1015l";

    /// Canonical SGR mouse enable sequence.
    pub const MOUSE_ENABLE: &[u8] = b"\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l\x1b[?1006;1000;1002h\x1b[?1006h\x1b[?1000h\x1b[?1002h";

    /// Conservative mouse enable sequence for mux sessions.
    pub const MOUSE_ENABLE_MUX_SAFE: &[u8] =
        b"\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l\x1b[?1006h\x1b[?1000h\x1b[?1002h";

    /// Show cursor (DECTCEM).
    pub const CURSOR_SHOW: &[u8] = b"\x1b[?25h";

    /// Hide cursor (DECTCEM).
    pub const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

    /// Leave alternate screen buffer (DECSET 1049).
    pub const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";

    /// Enter alternate screen buffer (DECSET 1049).
    pub const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
}

static KITTY_POP_LATCH: AtomicBool = AtomicBool::new(false);

/// Process-wide latch for stack-based Kitty keyboard pop emission.
///
/// Because Kitty keyboard mode is stack-based, popping it more than once per
/// process can pop an enclosing terminal or multiplexer's keyboard state.
/// This latch ensures that best-effort teardown paths emit at most one pop.
#[derive(Debug, Clone, Copy, Default)]
pub struct KittyPopLatch;

impl KittyPopLatch {
    /// Attempt to claim the one-shot kitty keyboard pop emission.
    ///
    /// Returns `true` if this caller claimed it (first caller), `false` if already claimed.
    #[inline]
    pub fn try_claim() -> bool {
        !KITTY_POP_LATCH.swap(true, Ordering::SeqCst)
    }

    /// Check if the latch has already been claimed.
    #[inline]
    pub fn is_claimed() -> bool {
        KITTY_POP_LATCH.load(Ordering::SeqCst)
    }

    /// Reset the latch for testing purposes.
    #[inline]
    pub fn reset_for_tests() {
        KITTY_POP_LATCH.store(false, Ordering::SeqCst);
    }
}

/// Declarative plan describing terminal restore escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeardownPlan {
    /// Emit synchronized output end sequence (`\x1b[?2026l`).
    pub emit_sync_end: bool,
    /// Reset scroll margins to full screen bracketed by DECSC/DECRC (`\x1b7\x1b[r\x1b8`).
    pub reset_scroll_region: bool,
    /// Reset text style/SGR attributes (`\x1b[0m`).
    pub reset_style: bool,
    /// Pop Kitty keyboard protocol stack (`\x1b[<u`).
    pub pop_kitty_keyboard: bool,
    /// Disable focus event reporting (`\x1b[?1004l`).
    pub disable_focus: bool,
    /// Disable bracketed paste mode (`\x1b[?2004l`).
    pub disable_paste: bool,
    /// Disable mouse tracking.
    pub disable_mouse: bool,
    /// Use mux-safe mouse disable sequence rather than full sequence.
    pub mouse_mux_safe: bool,
    /// Optional override for mouse disable byte sequence.
    pub mouse_disable_override: Option<&'static [u8]>,
    /// Show cursor (`\x1b[?25h`).
    pub show_cursor: bool,
    /// Leave alternate screen buffer (`\x1b[?1049l`).
    pub leave_alt_screen: bool,
}

impl TeardownPlan {
    /// Create a teardown plan from terminal capabilities and session options.
    ///
    /// Applies standard mux policies: kitty keyboard and focus events are
    /// disabled only if the capability is supported and not running inside a
    /// multiplexer (`!caps.in_any_mux()`).
    #[must_use]
    pub fn from_capabilities(
        caps: &TerminalCapabilities,
        alt_screen: bool,
        emit_sync_end: bool,
    ) -> Self {
        Self {
            emit_sync_end,
            reset_scroll_region: true,
            reset_style: true,
            pop_kitty_keyboard: caps.kitty_keyboard && !caps.in_any_mux(),
            disable_focus: caps.focus_events && !caps.in_any_mux(),
            disable_paste: caps.bracketed_paste,
            disable_mouse: caps.mouse_sgr,
            mouse_mux_safe: caps.in_any_mux(),
            mouse_disable_override: None,
            show_cursor: true,
            leave_alt_screen: alt_screen,
        }
    }

    /// Write all requested cleanup escape sequences in canonical order to `w`.
    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        self.write_for_backend(w, "unknown")
    }

    /// Write all requested cleanup escape sequences in canonical order to `w`,
    /// logging structured start and end events if the `tracing` feature is enabled.
    pub fn write_for_backend(
        &self,
        w: &mut impl io::Write,
        _backend: &'static str,
    ) -> io::Result<()> {
        #[cfg(feature = "tracing")]
        tracing::info!(
            backend = _backend,
            plan = ?self,
            "teardown start"
        );

        if self.emit_sync_end {
            w.write_all(seq::SYNC_END)?;
        }
        if self.reset_scroll_region {
            w.write_all(seq::DECSC)?;
            w.write_all(seq::RESET_SCROLL_REGION)?;
            w.write_all(seq::DECRC)?;
        }
        if self.reset_style {
            w.write_all(seq::RESET_STYLE)?;
        }
        if self.pop_kitty_keyboard {
            w.write_all(seq::KITTY_KEYBOARD_DISABLE)?;
        }
        if self.disable_focus {
            w.write_all(seq::FOCUS_DISABLE)?;
        }
        if self.disable_paste {
            w.write_all(seq::BRACKETED_PASTE_DISABLE)?;
        }
        if self.disable_mouse {
            let mouse_seq = self
                .mouse_disable_override
                .unwrap_or(if self.mouse_mux_safe {
                    seq::MOUSE_DISABLE_MUX_SAFE
                } else {
                    seq::MOUSE_DISABLE
                });
            w.write_all(mouse_seq)?;
        }
        if self.show_cursor {
            w.write_all(seq::CURSOR_SHOW)?;
        }
        if self.leave_alt_screen {
            w.write_all(seq::ALT_SCREEN_LEAVE)?;
        }

        #[cfg(feature = "tracing")]
        tracing::info!(backend = _backend, "teardown end");

        Ok(())
    }
}

static INSTALLED_HOOKS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Install a panic hook with the given identifier that chains to the previously
/// installed hook.
///
/// Guaranteed to install at most once per `name`. When a panic occurs, `f()` is
/// executed, followed by the previously installed panic hook.
pub fn install_chained_panic_hook(name: &'static str, f: fn()) {
    let mut installed = match INSTALLED_HOOKS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if installed.contains(&name) {
        return;
    }
    installed.push(name);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        f();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_emits_canonical_order() {
        let mut expected = Vec::new();
        // 1. sync_end
        expected.extend_from_slice(seq::SYNC_END);
        // 2. scroll region reset: ESC 7, CSI r, ESC 8
        expected.extend_from_slice(seq::DECSC);
        expected.extend_from_slice(seq::RESET_SCROLL_REGION);
        expected.extend_from_slice(seq::DECRC);
        // 3. SGR reset
        expected.extend_from_slice(seq::RESET_STYLE);
        // 4. kitty pop
        expected.extend_from_slice(seq::KITTY_KEYBOARD_DISABLE);
        // 5. focus disable
        expected.extend_from_slice(seq::FOCUS_DISABLE);
        // 6. bracketed paste disable
        expected.extend_from_slice(seq::BRACKETED_PASTE_DISABLE);
        // 7. mouse disable
        expected.extend_from_slice(seq::MOUSE_DISABLE);
        // 8. cursor show
        expected.extend_from_slice(seq::CURSOR_SHOW);
        // 9. alt-screen leave
        expected.extend_from_slice(seq::ALT_SCREEN_LEAVE);

        let plan = TeardownPlan {
            emit_sync_end: true,
            reset_scroll_region: true,
            reset_style: true,
            pop_kitty_keyboard: true,
            disable_focus: true,
            disable_paste: true,
            disable_mouse: true,
            mouse_mux_safe: false,
            mouse_disable_override: None,
            show_cursor: true,
            leave_alt_screen: true,
        };

        let mut buf = Vec::new();
        plan.write(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }

    #[test]
    fn latch_claims_once() {
        KittyPopLatch::reset_for_tests();
        assert!(!KittyPopLatch::is_claimed());
        assert!(KittyPopLatch::try_claim());
        assert!(KittyPopLatch::is_claimed());
        assert!(!KittyPopLatch::try_claim());
        assert!(!KittyPopLatch::try_claim());
        KittyPopLatch::reset_for_tests();
        assert!(!KittyPopLatch::is_claimed());
        assert!(KittyPopLatch::try_claim());
    }

    #[test]
    fn from_capabilities_applies_mux_policy() {
        let mut caps = TerminalCapabilities::modern();
        caps.kitty_keyboard = true;
        caps.focus_events = true;
        caps.bracketed_paste = true;
        caps.mouse_sgr = true;

        // Normal modern terminal: kitty and focus enabled, standard mouse disable.
        let plan = TeardownPlan::from_capabilities(&caps, true, true);
        assert!(plan.pop_kitty_keyboard);
        assert!(plan.disable_focus);
        assert!(plan.disable_paste);
        assert!(plan.disable_mouse);
        assert!(!plan.mouse_mux_safe);
        assert!(plan.leave_alt_screen);
        assert!(plan.emit_sync_end);

        // Under tmux: kitty and focus inhibited, mux-safe mouse disable.
        let tmux_caps = TerminalCapabilities::tmux();
        let tmux_plan = TeardownPlan::from_capabilities(&tmux_caps, true, false);
        assert!(!tmux_plan.pop_kitty_keyboard);
        assert!(!tmux_plan.disable_focus);
        assert!(tmux_plan.mouse_mux_safe);
        assert!(!tmux_plan.emit_sync_end);
    }

    #[test]
    fn chained_hook_runs_previous() {
        static HOOK_A_RAN: AtomicBool = AtomicBool::new(false);
        static HOOK_B_RAN: AtomicBool = AtomicBool::new(false);

        fn hook_a() {
            HOOK_A_RAN.store(true, Ordering::SeqCst);
        }
        fn hook_b() {
            HOOK_B_RAN.store(true, Ordering::SeqCst);
        }

        install_chained_panic_hook("test-hook-a", hook_a);
        install_chained_panic_hook("test-hook-b", hook_b);

        // Suppress default stderr output from panic hook during test.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let _ = std::panic::catch_unwind(|| {
            // Restore chained hook inside catch_unwind
        });

        // Re-install and trigger
        std::panic::set_hook(prev_hook);
        let _ = std::panic::catch_unwind(|| {
            panic!("test panic for chained hook");
        });

        assert!(HOOK_A_RAN.load(Ordering::SeqCst));
        assert!(HOOK_B_RAN.load(Ordering::SeqCst));
    }

    #[test]
    fn hook_runs_under_unwind() {
        static UNWIND_HOOK_RAN: AtomicBool = AtomicBool::new(false);
        static TEST_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

        fn session_hook() {
            if TEST_SESSION_ACTIVE.load(Ordering::SeqCst) {
                UNWIND_HOOK_RAN.store(true, Ordering::SeqCst);
            }
        }

        install_chained_panic_hook("test-unwind-hook", session_hook);

        TEST_SESSION_ACTIVE.store(true, Ordering::SeqCst);
        let handle = std::thread::spawn(|| {
            let _ = std::panic::catch_unwind(|| {
                panic!("trigger hook under unwind");
            });
        });
        let _ = handle.join();
        TEST_SESSION_ACTIVE.store(false, Ordering::SeqCst);

        assert!(
            UNWIND_HOOK_RAN.load(Ordering::SeqCst),
            "hook must run under panic=unwind when session is active"
        );
    }
}
