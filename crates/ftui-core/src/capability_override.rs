#![forbid(unsafe_code)]

//! Runtime capability override injection for testing (bd-k4lj.3).
//!
//! This module provides a thread-local override mechanism for terminal
//! capabilities, enabling tests to simulate various terminal environments
//! without modifying global state.
//!
//! # Overview
//!
//! - **Thread-local**: Overrides are scoped to the current thread, ensuring
//!   test isolation in parallel test runs.
//! - **Stackable**: Multiple overrides can be nested, with inner overrides
//!   taking precedence.
//! - **RAII-based**: Overrides are automatically removed when the guard is
//!   dropped, even on panic.
//!
//! # Invariants
//!
//! 1. **Thread isolation**: Overrides on one thread never affect another.
//! 2. **Stack ordering**: Later pushes override earlier ones; pops restore
//!    the previous state.
//! 3. **Cleanup guarantee**: Guards implement Drop to ensure cleanup even
//!    on panic or early return.
//! 4. **No runtime cost when unused**: If no overrides are active, capability
//!    resolution has minimal overhead (just checking the thread-local stack).
//!
//! # Failure Modes
//!
//! | Mode | Condition | Behavior |
//! |------|-----------|----------|
//! | Guard leaked | Guard moved without dropping | Override persists until thread exit |
//! | Stack underflow | Bug in guard management | Panics (debug) or no-op (release) |
//! | Thread exit | Thread terminates with active overrides | TLS destructor cleans up |
//!
//! # Example
//!
//! ```
//! use ftui_core::capability_override::{with_capability_override, CapabilityOverride};
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//!
//! // Simulate a dumb terminal
//! let override_cfg = CapabilityOverride::new()
//!     .true_color(Some(false))
//!     .colors_256(Some(false))
//!     .mouse_sgr(Some(false));
//!
//! with_capability_override(override_cfg, || {
//!     let caps = TerminalCapabilities::with_overrides();
//!     assert!(!caps.true_color);
//!     assert!(!caps.mouse_sgr);
//! });
//! ```

use crate::terminal_capabilities::TerminalCapabilities;
use std::cell::RefCell;

// ============================================================================
// Capability Override
// ============================================================================

/// Override specification for terminal capabilities.
///
/// Each field is `Option<bool>`:
/// - `Some(true)` - Force capability ON
/// - `Some(false)` - Force capability OFF
/// - `None` - Don't override (use base or previous override)
#[derive(Debug, Clone, Default)]
pub struct CapabilityOverride {
    // Color
    pub true_color: Option<bool>,
    pub colors_256: Option<bool>,

    // Glyph support
    pub unicode_box_drawing: Option<bool>,
    pub unicode_emoji: Option<bool>,
    pub double_width: Option<bool>,

    // Advanced features
    pub sync_output: Option<bool>,
    pub osc8_hyperlinks: Option<bool>,
    pub scroll_region: Option<bool>,

    // Multiplexer flags
    pub in_tmux: Option<bool>,
    pub in_screen: Option<bool>,
    pub in_zellij: Option<bool>,

    // Input features
    pub kitty_keyboard: Option<bool>,
    pub focus_events: Option<bool>,
    pub bracketed_paste: Option<bool>,
    pub mouse_sgr: Option<bool>,

    // Optional features
    pub osc52_clipboard: Option<bool>,
}

impl CapabilityOverride {
    /// Create a new empty override (no fields overridden).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            true_color: None,
            colors_256: None,
            unicode_box_drawing: None,
            unicode_emoji: None,
            double_width: None,
            sync_output: None,
            osc8_hyperlinks: None,
            scroll_region: None,
            in_tmux: None,
            in_screen: None,
            in_zellij: None,
            kitty_keyboard: None,
            focus_events: None,
            bracketed_paste: None,
            mouse_sgr: None,
            osc52_clipboard: None,
        }
    }

    /// Create an override that disables all capabilities (dumb terminal).
    #[must_use]
    pub const fn dumb() -> Self {
        Self {
            true_color: Some(false),
            colors_256: Some(false),
            unicode_box_drawing: Some(false),
            unicode_emoji: Some(false),
            double_width: Some(false),
            sync_output: Some(false),
            osc8_hyperlinks: Some(false),
            scroll_region: Some(false),
            in_tmux: Some(false),
            in_screen: Some(false),
            in_zellij: Some(false),
            kitty_keyboard: Some(false),
            focus_events: Some(false),
            bracketed_paste: Some(false),
            mouse_sgr: Some(false),
            osc52_clipboard: Some(false),
        }
    }

    /// Create an override that enables all capabilities (modern terminal).
    #[must_use]
    pub const fn modern() -> Self {
        Self {
            true_color: Some(true),
            colors_256: Some(true),
            unicode_box_drawing: Some(true),
            unicode_emoji: Some(true),
            double_width: Some(true),
            sync_output: Some(true),
            osc8_hyperlinks: Some(true),
            scroll_region: Some(true),
            in_tmux: Some(false),
            in_screen: Some(false),
            in_zellij: Some(false),
            kitty_keyboard: Some(true),
            focus_events: Some(true),
            bracketed_paste: Some(true),
            mouse_sgr: Some(true),
            osc52_clipboard: Some(true),
        }
    }

    /// Create an override that simulates running inside tmux.
    #[must_use]
    pub const fn tmux() -> Self {
        Self {
            true_color: None,
            colors_256: Some(true),
            unicode_box_drawing: None,
            unicode_emoji: None,
            double_width: None,
            sync_output: Some(false),
            osc8_hyperlinks: Some(false),
            scroll_region: Some(true),
            in_tmux: Some(true),
            in_screen: Some(false),
            in_zellij: Some(false),
            kitty_keyboard: Some(false),
            focus_events: Some(false),
            bracketed_paste: Some(true),
            mouse_sgr: Some(true),
            osc52_clipboard: Some(false),
        }
    }

    // ── Builder Methods ────────────────────────────────────────────────

    /// Override true color support.
    #[must_use]
    pub const fn true_color(mut self, value: Option<bool>) -> Self {
        self.true_color = value;
        self
    }

    /// Override 256-color support.
    #[must_use]
    pub const fn colors_256(mut self, value: Option<bool>) -> Self {
        self.colors_256 = value;
        self
    }

    /// Override Unicode box drawing support.
    #[must_use]
    pub const fn unicode_box_drawing(mut self, value: Option<bool>) -> Self {
        self.unicode_box_drawing = value;
        self
    }

    /// Override emoji glyph support.
    #[must_use]
    pub const fn unicode_emoji(mut self, value: Option<bool>) -> Self {
        self.unicode_emoji = value;
        self
    }

    /// Override double-width glyph support.
    #[must_use]
    pub const fn double_width(mut self, value: Option<bool>) -> Self {
        self.double_width = value;
        self
    }

    /// Override synchronized output support.
    #[must_use]
    pub const fn sync_output(mut self, value: Option<bool>) -> Self {
        self.sync_output = value;
        self
    }

    /// Override OSC 8 hyperlinks support.
    #[must_use]
    pub const fn osc8_hyperlinks(mut self, value: Option<bool>) -> Self {
        self.osc8_hyperlinks = value;
        self
    }

    /// Override scroll region support.
    #[must_use]
    pub const fn scroll_region(mut self, value: Option<bool>) -> Self {
        self.scroll_region = value;
        self
    }

    /// Override tmux detection.
    #[must_use]
    pub const fn in_tmux(mut self, value: Option<bool>) -> Self {
        self.in_tmux = value;
        self
    }

    /// Override GNU screen detection.
    #[must_use]
    pub const fn in_screen(mut self, value: Option<bool>) -> Self {
        self.in_screen = value;
        self
    }

    /// Override Zellij detection.
    #[must_use]
    pub const fn in_zellij(mut self, value: Option<bool>) -> Self {
        self.in_zellij = value;
        self
    }

    /// Override Kitty keyboard protocol support.
    #[must_use]
    pub const fn kitty_keyboard(mut self, value: Option<bool>) -> Self {
        self.kitty_keyboard = value;
        self
    }

    /// Override focus events support.
    #[must_use]
    pub const fn focus_events(mut self, value: Option<bool>) -> Self {
        self.focus_events = value;
        self
    }

    /// Override bracketed paste mode support.
    #[must_use]
    pub const fn bracketed_paste(mut self, value: Option<bool>) -> Self {
        self.bracketed_paste = value;
        self
    }

    /// Override SGR mouse protocol support.
    #[must_use]
    pub const fn mouse_sgr(mut self, value: Option<bool>) -> Self {
        self.mouse_sgr = value;
        self
    }

    /// Override OSC 52 clipboard support.
    #[must_use]
    pub const fn osc52_clipboard(mut self, value: Option<bool>) -> Self {
        self.osc52_clipboard = value;
        self
    }

    /// Check if any capability is overridden.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.true_color.is_none()
            && self.colors_256.is_none()
            && self.unicode_box_drawing.is_none()
            && self.unicode_emoji.is_none()
            && self.double_width.is_none()
            && self.sync_output.is_none()
            && self.osc8_hyperlinks.is_none()
            && self.scroll_region.is_none()
            && self.in_tmux.is_none()
            && self.in_screen.is_none()
            && self.in_zellij.is_none()
            && self.kitty_keyboard.is_none()
            && self.focus_events.is_none()
            && self.bracketed_paste.is_none()
            && self.mouse_sgr.is_none()
            && self.osc52_clipboard.is_none()
    }

    /// Apply this override on top of base capabilities.
    #[must_use]
    pub fn apply_to(&self, mut caps: TerminalCapabilities) -> TerminalCapabilities {
        if let Some(v) = self.true_color {
            caps.true_color = v;
        }
        if let Some(v) = self.colors_256 {
            caps.colors_256 = v;
        }
        if let Some(v) = self.unicode_box_drawing {
            caps.unicode_box_drawing = v;
        }
        if let Some(v) = self.unicode_emoji {
            caps.unicode_emoji = v;
        }
        if let Some(v) = self.double_width {
            caps.double_width = v;
        }
        if let Some(v) = self.sync_output {
            caps.sync_output = v;
        }
        if let Some(v) = self.osc8_hyperlinks {
            caps.osc8_hyperlinks = v;
        }
        if let Some(v) = self.scroll_region {
            caps.scroll_region = v;
        }
        if let Some(v) = self.in_tmux {
            caps.in_tmux = v;
        }
        if let Some(v) = self.in_screen {
            caps.in_screen = v;
        }
        if let Some(v) = self.in_zellij {
            caps.in_zellij = v;
        }
        if let Some(v) = self.kitty_keyboard {
            caps.kitty_keyboard = v;
        }
        if let Some(v) = self.focus_events {
            caps.focus_events = v;
        }
        if let Some(v) = self.bracketed_paste {
            caps.bracketed_paste = v;
        }
        if let Some(v) = self.mouse_sgr {
            caps.mouse_sgr = v;
        }
        if let Some(v) = self.osc52_clipboard {
            caps.osc52_clipboard = v;
        }
        caps
    }
}

// ============================================================================
// Thread-Local Override Stack
// ============================================================================

thread_local! {
    /// Stack of active capability overrides for this thread.
    static OVERRIDE_STACK: RefCell<Vec<CapabilityOverride>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard that removes an override when dropped.
///
/// Do not leak this guard - it must be dropped to restore the previous state.
#[must_use]
pub struct OverrideGuard {
    /// Marker to prevent Send/Sync (thread-local data)
    _marker: std::marker::PhantomData<*const ()>,
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        // Silently ignore if stack is empty - this can happen if clear_all_overrides()
        // was called while guards were still active. This is documented behavior.
        OVERRIDE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push an override onto the thread-local stack.
///
/// Returns a guard that will pop the override when dropped.
///
/// # Example
///
/// ```
/// use ftui_core::capability_override::{push_override, CapabilityOverride};
///
/// let _guard = push_override(CapabilityOverride::dumb());
/// // Override is active here
/// // Automatically removed when _guard is dropped
/// ```
#[must_use = "the override is removed when the guard is dropped"]
pub fn push_override(over: CapabilityOverride) -> OverrideGuard {
    OVERRIDE_STACK.with(|stack| {
        stack.borrow_mut().push(over);
    });
    OverrideGuard {
        _marker: std::marker::PhantomData,
    }
}

/// Execute a closure with a capability override active.
///
/// The override is automatically removed when the closure returns,
/// even if it panics.
///
/// # Example
///
/// ```
/// use ftui_core::capability_override::{with_capability_override, CapabilityOverride};
/// use ftui_core::terminal_capabilities::TerminalCapabilities;
///
/// with_capability_override(CapabilityOverride::dumb(), || {
///     let caps = TerminalCapabilities::with_overrides();
///     assert!(!caps.true_color);
/// });
/// ```
pub fn with_capability_override<F, R>(over: CapabilityOverride, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = push_override(over);
    f()
}

/// Get the current effective capabilities with all overrides applied.
///
/// This starts with `TerminalCapabilities::detect()` and applies each
/// override in the stack from bottom to top.
#[must_use]
pub fn current_capabilities() -> TerminalCapabilities {
    let base = TerminalCapabilities::detect();
    current_capabilities_with_base(base)
}

/// Get effective capabilities starting from a specified base.
#[must_use]
pub fn current_capabilities_with_base(base: TerminalCapabilities) -> TerminalCapabilities {
    OVERRIDE_STACK.with(|stack| {
        let stack = stack.borrow();
        stack.iter().fold(base, |caps, over| over.apply_to(caps))
    })
}

/// Check if any overrides are currently active on this thread.
#[must_use]
pub fn has_active_overrides() -> bool {
    OVERRIDE_STACK.with(|stack| !stack.borrow().is_empty())
}

/// Get the number of active overrides on this thread.
#[must_use]
pub fn override_depth() -> usize {
    OVERRIDE_STACK.with(|stack| stack.borrow().len())
}

/// Clear all overrides on this thread.
///
/// **Warning**: This bypasses RAII guards and should only be used for
/// cleanup in test harnesses, not in production code.
pub fn clear_all_overrides() {
    OVERRIDE_STACK.with(|stack| {
        stack.borrow_mut().clear();
    });
}

// ============================================================================
// Extension to TerminalCapabilities
// ============================================================================

impl TerminalCapabilities {
    /// Detect capabilities and apply any active thread-local overrides.
    ///
    /// This is the recommended way to get capabilities in code that may
    /// be running under test with overrides.
    #[must_use]
    pub fn with_overrides() -> Self {
        current_capabilities()
    }

    /// Apply overrides to these capabilities.
    #[must_use]
    pub fn with_overrides_from(self, base: Self) -> Self {
        current_capabilities_with_base(base)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_new_is_empty() {
        let over = CapabilityOverride::new();
        assert!(over.is_empty());
    }

    #[test]
    fn override_dumb_disables_all() {
        let over = CapabilityOverride::dumb();
        assert!(!over.is_empty());
        assert_eq!(over.true_color, Some(false));
        assert_eq!(over.colors_256, Some(false));
        assert_eq!(over.sync_output, Some(false));
        assert_eq!(over.mouse_sgr, Some(false));
    }

    #[test]
    fn override_modern_enables_all() {
        let over = CapabilityOverride::modern();
        assert_eq!(over.true_color, Some(true));
        assert_eq!(over.colors_256, Some(true));
        assert_eq!(over.sync_output, Some(true));
        assert_eq!(over.kitty_keyboard, Some(true));
        // But mux flags are false
        assert_eq!(over.in_tmux, Some(false));
    }

    #[test]
    fn override_tmux_sets_mux() {
        let over = CapabilityOverride::tmux();
        assert_eq!(over.in_tmux, Some(true));
        assert_eq!(over.sync_output, Some(false));
        assert_eq!(over.osc52_clipboard, Some(false));
    }

    #[test]
    fn override_builder_chain() {
        let over = CapabilityOverride::new()
            .true_color(Some(true))
            .colors_256(Some(true))
            .unicode_box_drawing(Some(false))
            .mouse_sgr(Some(false));

        assert_eq!(over.true_color, Some(true));
        assert_eq!(over.colors_256, Some(true));
        assert_eq!(over.unicode_box_drawing, Some(false));
        assert_eq!(over.mouse_sgr, Some(false));
        assert!(over.sync_output.is_none());
    }

    #[test]
    fn apply_to_overrides_caps() {
        let base = TerminalCapabilities::dumb();
        let over = CapabilityOverride::new()
            .true_color(Some(true))
            .colors_256(Some(true))
            .unicode_box_drawing(Some(true));

        let result = over.apply_to(base);
        assert!(result.true_color);
        assert!(result.colors_256);
        assert!(result.unicode_box_drawing);
        // Unchanged fields remain from base
        assert!(!result.mouse_sgr);
    }

    #[test]
    fn apply_to_none_keeps_original() {
        let base = TerminalCapabilities::modern();
        let over = CapabilityOverride::new(); // All None

        let result = over.apply_to(base);
        assert_eq!(result.true_color, base.true_color);
        assert_eq!(result.mouse_sgr, base.mouse_sgr);
    }

    #[test]
    fn push_pop_override() {
        clear_all_overrides();
        assert!(!has_active_overrides());
        assert_eq!(override_depth(), 0);

        {
            let _guard = push_override(CapabilityOverride::dumb());
            assert!(has_active_overrides());
            assert_eq!(override_depth(), 1);
        }

        assert!(!has_active_overrides());
        assert_eq!(override_depth(), 0);
    }

    #[test]
    fn nested_overrides() {
        clear_all_overrides();

        {
            let _outer = push_override(
                CapabilityOverride::new()
                    .true_color(Some(true))
                    .mouse_sgr(Some(true)),
            );
            assert_eq!(override_depth(), 1);

            {
                let _inner = push_override(CapabilityOverride::new().true_color(Some(false)));
                assert_eq!(override_depth(), 2);

                // Inner override takes precedence
                let caps = current_capabilities_with_base(TerminalCapabilities::dumb());
                assert!(!caps.true_color); // Inner: false
                assert!(caps.mouse_sgr); // Outer: true
            }

            // Inner dropped, outer still active
            assert_eq!(override_depth(), 1);
            let caps = current_capabilities_with_base(TerminalCapabilities::dumb());
            assert!(caps.true_color); // Outer: true
        }

        assert_eq!(override_depth(), 0);
    }

    #[test]
    fn with_capability_override_scope() {
        clear_all_overrides();

        let result = with_capability_override(CapabilityOverride::modern(), || {
            assert!(has_active_overrides());
            let caps = current_capabilities_with_base(TerminalCapabilities::dumb());
            caps.true_color
        });

        assert!(result);
        assert!(!has_active_overrides());
    }

    #[test]
    fn with_capability_override_nested() {
        clear_all_overrides();

        with_capability_override(CapabilityOverride::new().true_color(Some(true)), || {
            with_capability_override(CapabilityOverride::new().mouse_sgr(Some(false)), || {
                let caps = current_capabilities_with_base(TerminalCapabilities::dumb());
                assert!(caps.true_color);
                assert!(!caps.mouse_sgr);
            });
        });
    }

    #[test]
    fn with_overrides_method() {
        clear_all_overrides();

        with_capability_override(CapabilityOverride::dumb(), || {
            let caps = TerminalCapabilities::with_overrides();
            assert!(!caps.true_color);
            assert!(!caps.colors_256);
            assert!(!caps.unicode_box_drawing);
            assert!(!caps.unicode_emoji);
            assert!(!caps.double_width);
        });
    }

    #[test]
    fn clear_all_overrides_works() {
        let _g1 = push_override(CapabilityOverride::dumb());
        let _g2 = push_override(CapabilityOverride::modern());
        assert_eq!(override_depth(), 2);

        clear_all_overrides();
        assert_eq!(override_depth(), 0);
    }

    #[test]
    fn default_override_is_empty() {
        let over = CapabilityOverride::default();
        assert!(over.is_empty());
    }

    #[test]
    fn is_empty_false_for_single_override() {
        let over = CapabilityOverride::new().true_color(Some(true));
        assert!(!over.is_empty());
    }

    #[test]
    fn dumb_disables_all_fields() {
        let over = CapabilityOverride::dumb();
        assert_eq!(over.unicode_box_drawing, Some(false));
        assert_eq!(over.unicode_emoji, Some(false));
        assert_eq!(over.double_width, Some(false));
        assert_eq!(over.osc8_hyperlinks, Some(false));
        assert_eq!(over.scroll_region, Some(false));
        assert_eq!(over.kitty_keyboard, Some(false));
        assert_eq!(over.focus_events, Some(false));
        assert_eq!(over.bracketed_paste, Some(false));
        assert_eq!(over.osc52_clipboard, Some(false));
        assert_eq!(over.in_tmux, Some(false));
        assert_eq!(over.in_screen, Some(false));
        assert_eq!(over.in_zellij, Some(false));
    }

    #[test]
    fn modern_enables_features_disables_mux() {
        let over = CapabilityOverride::modern();
        assert_eq!(over.unicode_box_drawing, Some(true));
        assert_eq!(over.unicode_emoji, Some(true));
        assert_eq!(over.double_width, Some(true));
        assert_eq!(over.osc8_hyperlinks, Some(true));
        assert_eq!(over.scroll_region, Some(true));
        assert_eq!(over.focus_events, Some(true));
        assert_eq!(over.bracketed_paste, Some(true));
        assert_eq!(over.osc52_clipboard, Some(true));
        assert_eq!(over.in_screen, Some(false));
        assert_eq!(over.in_zellij, Some(false));
    }

    #[test]
    fn tmux_sets_bracketed_paste_and_colors() {
        let over = CapabilityOverride::tmux();
        assert_eq!(over.colors_256, Some(true));
        assert_eq!(over.bracketed_paste, Some(true));
        assert_eq!(over.mouse_sgr, Some(true));
        assert_eq!(over.scroll_region, Some(true));
        assert_eq!(over.kitty_keyboard, Some(false));
    }

    #[test]
    fn builder_all_optional_features() {
        let over = CapabilityOverride::new()
            .unicode_emoji(Some(true))
            .double_width(Some(false))
            .in_screen(Some(true))
            .in_zellij(Some(true))
            .osc8_hyperlinks(Some(true))
            .osc52_clipboard(Some(false))
            .scroll_region(Some(true))
            .focus_events(Some(true))
            .bracketed_paste(Some(false))
            .kitty_keyboard(Some(true));

        assert_eq!(over.unicode_emoji, Some(true));
        assert_eq!(over.double_width, Some(false));
        assert_eq!(over.in_screen, Some(true));
        assert_eq!(over.in_zellij, Some(true));
        assert_eq!(over.osc8_hyperlinks, Some(true));
        assert_eq!(over.osc52_clipboard, Some(false));
        assert_eq!(over.scroll_region, Some(true));
        assert_eq!(over.focus_events, Some(true));
        assert_eq!(over.bracketed_paste, Some(false));
        assert_eq!(over.kitty_keyboard, Some(true));
    }

    #[test]
    fn apply_to_covers_all_mux_flags() {
        let base = TerminalCapabilities::dumb();
        let over = CapabilityOverride::new()
            .in_tmux(Some(true))
            .in_screen(Some(true))
            .in_zellij(Some(true));
        let result = over.apply_to(base);
        assert!(result.in_tmux);
        assert!(result.in_screen);
        assert!(result.in_zellij);
    }

    #[test]
    fn apply_to_covers_input_features() {
        let base = TerminalCapabilities::dumb();
        let over = CapabilityOverride::new()
            .kitty_keyboard(Some(true))
            .focus_events(Some(true))
            .bracketed_paste(Some(true))
            .osc52_clipboard(Some(true));
        let result = over.apply_to(base);
        assert!(result.kitty_keyboard);
        assert!(result.focus_events);
        assert!(result.bracketed_paste);
        assert!(result.osc52_clipboard);
    }

    #[test]
    fn current_capabilities_with_base_composes_stack() {
        clear_all_overrides();
        let base = TerminalCapabilities::dumb();

        let _g1 = push_override(CapabilityOverride::new().true_color(Some(true)));
        let _g2 = push_override(CapabilityOverride::new().mouse_sgr(Some(true)));

        let caps = current_capabilities_with_base(base);
        assert!(caps.true_color);
        assert!(caps.mouse_sgr);
        assert!(!caps.colors_256); // Not overridden, remains dumb

        clear_all_overrides();
    }

    #[test]
    fn override_clone() {
        let over = CapabilityOverride::new()
            .true_color(Some(true))
            .in_tmux(Some(false));
        let cloned = over.clone();
        assert_eq!(over.true_color, cloned.true_color);
        assert_eq!(over.in_tmux, cloned.in_tmux);
    }
}
